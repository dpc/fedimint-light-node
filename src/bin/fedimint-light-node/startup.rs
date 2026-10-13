//! Normal startup composes fresh bootstrap or pinned reopen with read-only serving.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;

use crate::options::{Command, GuardianState, Options};
use crate::{Result, operations};

/// Open persistent local state, bootstrap only an absent destination, and serve.
pub async fn run(
    mut options: Options,
    mut shutdown: Pin<&mut impl Future<Output = io::Result<()>>>,
) -> Result<()> {
    if options.guardian_id.is_some() {
        return Err("normal startup is unbound; use serve with explicit guardian preflight".into());
    }
    let bind = options.rpc.rpc_bind;
    if !bind.ip().is_loopback() {
        return Err("RPC requires a numeric loopback address".into());
    }
    if let Some(credentials) = &options.rpc.rpc_credentials {
        // Report credential-file mistakes before any first-run network work.
        crate::rpc_credentials::read(credentials)?;
    }
    if options.db.is_none() {
        let db = default_db()?;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(db.parent().expect("default database has a parent"))?;
        options.db = Some(db);
    }
    let db = options.db.as_ref().expect("database path resolved");
    eprintln!(
        "database={} (persistent; never reset automatically)",
        db.display()
    );
    // A directory, symlink (including dangling), partial or invalid existing file
    // is NOT an absent database. Bootstrap itself publishes without overwrite.
    let absent = match std::fs::symlink_metadata(db) {
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    if !absent {
        let store = fedimint_light_node::validated_header_store::ValidatedHeaderStore::open_pinned(
            db,
            options.store_config(),
        )?;
        crate::recovery::identity(&store, None).map_err(|error| {
            format!("normal startup requires unbound state: {error}; use serve with explicit guardian preflight")
        })?;
    }
    // Fail before bootstrap on an occupied port. The actual bind after bootstrap
    // also checks for races; port zero is selected at that point.
    drop(tokio::net::TcpListener::bind(bind).await.map_err(|error| {
        io::Error::new(error.kind(), format!("cannot bind RPC at {bind}: {error}; use --rpc-bind 127.0.0.1:0 to choose an available port"))
    })?);
    if absent {
        eprintln!(
            "initializing new mainnet state; HTTPS source trust, NOT full consensus validation"
        );
        options.command = Some(Command::Bootstrap {
            network: options.network.clone(),
            source: Vec::new(),
            quorum: 2,
            confirmation_margin: 6,
            total_seconds: 600,
        });
        operations::run(options.clone(), shutdown.as_mut()).await?;
    }
    let mut rpc = options.rpc.clone();
    if rpc.rpc_credentials.is_none() {
        rpc.rpc_no_auth = true;
    }
    options.command = Some(Command::Serve {
        network: options.network.clone(),
        rpc,
        standalone: true,
        guardian: GuardianState::default(),
    });
    operations::run(options, shutdown).await
}

/// Resolve a stable per-user state path without falling back to the current directory.
fn default_db() -> Result<PathBuf> {
    state_path(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// Use absolute XDG state paths, or the standard HOME-relative fallback.
pub(crate) fn state_path(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    let root = match xdg.filter(|path| !path.is_empty()) {
        Some(path) if PathBuf::from(&path).is_absolute() => PathBuf::from(path),
        _ => {
            let home = PathBuf::from(home.ok_or("set HOME, XDG_STATE_HOME or --db")?);
            if !home.is_absolute() {
                return Err("HOME must be absolute; alternatively supply --db".into());
            }
            home.join(".local/state")
        }
    };
    Ok(root.join("fedimint-light-node/chain.redb"))
}
