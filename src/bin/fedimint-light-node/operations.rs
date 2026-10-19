use std::future::Future;
use std::io;
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fedimint_light_node::block_decode::BlockDecodeLimits;
use fedimint_light_node::block_download_config::BlockDownloadConfig;
use fedimint_light_node::bootstrap::bootstrap;
use fedimint_light_node::bootstrap_config::BootstrapConfig;
use fedimint_light_node::header_sync::HeaderSync;
use fedimint_light_node::transaction_submission_config::TransactionSubmissionConfig;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;

use crate::options::{Command, Options, RpcOptions};
use crate::{Result, interrupted, output};

/// Dispatch explicit operations without adding chain logic or implicit recovery.
pub async fn run(
    options: Options,
    mut shutdown: Pin<&mut impl Future<Output = io::Result<()>>>,
) -> Result<()> {
    let config = options.store_config();
    let fee_policy = options.fee_policy();
    let db = options
        .db
        .as_deref()
        .ok_or("--db is required for explicit commands")?;
    let mut peer_cache = db.as_os_str().to_os_string();
    peer_cache.push(".peers");
    let peer_cache = std::path::PathBuf::from(peer_cache);
    output::fee_policy(fee_policy)?;
    match options.command.ok_or("explicit command required")? {
        Command::Bootstrap {
            network,
            source,
            quorum,
            confirmation_margin,
            total_seconds,
        } => {
            let mut config = BootstrapConfig {
                peer_pool: {
                    let mut pool = network.pool_config();
                    pool.cache = Some(peer_cache.clone());
                    pool
                },
                store: config,
                peers: network.peer,
                quorum: quorum as usize,
                confirmation_margin,
                request_timeout: Duration::from_secs(network.timeout_seconds),
                total_timeout: Duration::from_secs(total_seconds),
                ..BootstrapConfig::default()
            };
            if !source.is_empty() {
                config.sources = source;
            }
            tracing::warn!(
                "bootstrap: trusting configured source agreement; no consensus/freshness guarantee"
            );
            let store = tokio::select! {
                biased;
                result = &mut shutdown => { result?; return Err(interrupted()); }
                result = bootstrap(db, config, now()?) => result?,
            };
            output::status(&store)?;
        }
        Command::Status => {
            output::status(&ValidatedHeaderStore::open_pinned(db, config)?)?;
        }
        Command::Checkpoint { output } => {
            let store = ValidatedHeaderStore::open_pinned(db, config)?;
            crate::recovery::identity(&store, options.guardian_id)?;
            crate::recovery::write_json(&output, &crate::recovery::state(&store)?)?;
        }
        Command::Backup { output, manifest } => {
            let store = ValidatedHeaderStore::open_pinned(db, config)?;
            crate::recovery::identity(&store, options.guardian_id)?;
            crate::recovery::backup(&store, config, &output, &manifest)?;
        }
        Command::Restore {
            backup,
            manifest,
            expected_state,
        } => {
            crate::recovery::restore(
                &backup,
                &manifest,
                &expected_state,
                db,
                config,
                options.guardian_id,
            )?;
        }
        Command::BindGuardian { guardian } => {
            let mut store = ValidatedHeaderStore::open_pinned(db, config)?;
            store.bind_guardian(guardian.preflight(options.guardian_id)?)?;
            output::status(&store)?;
        }
        Command::Serve {
            network,
            rpc:
                RpcOptions {
                    mempool,
                    rpc_bind,
                    rpc_credentials,
                    rpc_no_auth,
                    fake_bitcoind,
                    allow_rpc_submission,
                },
            standalone,
            guardian,
        } => {
            use fedimint_light_node::guardian_rpc::{GuardianRpc, SYNTHETIC_STATUS_WARNING};
            use fedimint_light_node::guardian_rpc_config::{GuardianRpcConfig, RpcAuth};
            let auth = if rpc_no_auth {
                RpcAuth::NoAuth
            } else {
                let (username, password) = crate::rpc_credentials::read(
                    rpc_credentials
                        .as_deref()
                        .ok_or("serve requires --rpc-credentials or --rpc-no-auth")?,
                )?;
                RpcAuth::Basic { username, password }
            };
            let store = ValidatedHeaderStore::open_pinned(db, config)?;
            if standalone {
                if options.guardian_id.is_some() || guardian.is_present() {
                    return Err("--standalone cannot include guardian state".into());
                }
                crate::recovery::identity(&store, None)?;
                tracing::info!(mode = "standalone", guardian_coordination = "absent");
            } else {
                store.preflight_guardian(guardian.preflight(options.guardian_id)?)?;
                tracing::info!(
                    mode = "bound-guardian-manual",
                    preflight = "passed",
                    automatic_commit_coordination = "absent"
                );
            }
            output::status(&store)?;
            let mut sync_config =
                network.sync_config(config.header_capacity.get(), 1, 8_000, 1, peer_cache);
            // Fit a visit in the transport's fairness slice; no long Core-client waits.
            sync_config.retry_delay = Duration::from_millis(100);
            sync_config.handshake_timeout = Duration::from_secs(3);
            sync_config.request_timeout = Duration::from_secs(3);
            let sync = HeaderSync::new_with_mempool(store, sync_config, mempool)?
                .with_fee_policy(fee_policy);
            let rpc = GuardianRpc::bind(
                GuardianRpcConfig {
                    bind: rpc_bind,
                    auth,
                    fake_bitcoind,
                    allow_submission: allow_rpc_submission,
                },
                sync,
            )
            .await?;
            tracing::info!(event = "rpc_listening", rpc_listen = %rpc.local_addr()?,
                rpc_no_auth, fake_bitcoind, submission_only = allow_rpc_submission,
                "RPC listener ready");
            if rpc_no_auth {
                let addr = rpc.local_addr()?;
                tracing::warn!(
                    "read-only local RPC; any local process can query; do not forward this port"
                );
                tracing::info!(
                    "bitcoin-cli -datadir=/tmp -conf=/dev/null -rpcconnect={} -rpcport={} getblockcount",
                    addr.ip(),
                    addr.port()
                );
                tracing::info!(
                    "curl --noproxy '*' -H 'Content-Type: application/json' --data '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getblockcount\",\"params\":[]}}' 'http://{addr}/'"
                );
            }
            if fake_bitcoind {
                tracing::warn!("{SYNTHETIC_STATUS_WARNING}");
            }
            if mempool {
                tracing::warn!(
                    "mempool observation is approximate, incomplete and unvalidated; latest 20 selected bodies are downloaded under bounded background budgets; presence is not acceptance, absence is not confirmation"
                );
            }
            tracing::warn!(
                "guardian protection is NOT automatic; reads never advance durable protection"
            );
            if allow_rpc_submission {
                tracing::warn!(
                    "WARNING: submission-only; no input fee/burn checks, mempool or acceptance guarantee; cancellation may follow delivery"
                );
            }
            let mut progress = crate::sync_progress::SyncProgress::default();
            rpc.run(
                async {
                    let _ = shutdown.await;
                },
                |sync, diagnostic| {
                    if let Err(error) = progress.report(sync) {
                        tracing::error!(%error, "cannot read local synchronization state");
                    }
                    tracing::debug!(event = "sync_visit", diagnostic, "owner visit");
                },
            )
            .await?;
            tracing::info!(event = "rpc_stopped", "RPC owner and peer pool stopped");
        }
        Command::Broadcast {
            network,
            input,
            retry_seconds,
            total_seconds,
        } => {
            let limits = TransactionSubmissionConfig {
                total_timeout: Duration::from_secs(total_seconds),
                visit_timeout: Duration::from_secs(network.timeout_seconds),
                ..TransactionSubmissionConfig::default()
            };
            let transaction = crate::transaction_input::read(&input, limits)?;
            let store = ValidatedHeaderStore::open_pinned(db, config)?;
            let mut sync = HeaderSync::new(
                store,
                network.sync_config(
                    config.header_capacity.get(),
                    4,
                    8_000,
                    retry_seconds,
                    peer_cache.clone(),
                ),
            )?
            .with_fee_policy(fee_policy);
            tracing::warn!(
                "submission is a local write only; cancellation may follow delivery; explicit retry is allowed"
            );
            let report = tokio::select! {
                biased;
                result = &mut shutdown => {
                    result?;
                    sync.shutdown().await;
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "submission interrupted; delivery is indeterminate, explicit retry is allowed",
                    ).into());
                }
                result = sync.submit_transaction(&transaction, limits) => result?,
            };
            sync.shutdown().await;
            output::submission(&report)?;
            if report.submitted() == 0 {
                return Err(
                    "no completed transaction submission; started writes may still have delivered"
                        .into(),
                );
            }
        }
        Command::Protect { height, hash } => {
            let mut store = ValidatedHeaderStore::open_pinned(db, config)?;
            crate::recovery::identity(&store, options.guardian_id)?;
            store.protect_through(height, hash)?;
            output::status(&store)?;
        }
        Command::Sync {
            network,
            retry_seconds,
            once,
            requests,
            candidate_headers,
        } => {
            let store = ValidatedHeaderStore::open_pinned(db, config)?;
            let mut sync = HeaderSync::new(
                store,
                network.sync_config(
                    config.header_capacity.get(),
                    requests,
                    candidate_headers,
                    retry_seconds,
                    peer_cache.clone(),
                ),
            )?
            .with_fee_policy(fee_policy);
            let mut progress = crate::sync_progress::SyncProgress::default();
            loop {
                let report = tokio::select! {
                    biased;
                    result = &mut shutdown => {
                        result?;
                        sync.shutdown().await;
                        output::status(sync.store())?;
                        return Err(interrupted());
                    }
                    report = sync.step(now()?) => report,
                };
                progress.report(&sync)?;
                match report.result {
                    Ok(end) => {
                        tracing::debug!(?end, peer = ?report.peer, "visit completed; freshness unproven")
                    }
                    Err(error) => {
                        tracing::debug!(%error, peer = ?report.peer, "visit failed");
                        if once || error.is_terminal() {
                            sync.shutdown().await;
                            output::status(sync.store())?;
                            return Err(error.into());
                        }
                    }
                }
                if once {
                    sync.shutdown().await;
                    output::status(sync.store())?;
                    break;
                }
            }
        }
        Command::Fetch {
            network,
            retry_seconds,
            height,
            hash,
            output: path,
            total_seconds,
        } => {
            // Even aliases of the DB (hard links/symlinks) already exist and fail
            // here. Publication checks again atomically after network work.
            output::require_new(&path)?;
            let store = ValidatedHeaderStore::open_pinned(db, config)?;
            let hash = match hash {
                Some(hash) => hash,
                None => store.header(height)?.block_hash(),
            };
            let mut sync = HeaderSync::new(
                store,
                network.sync_config(
                    config.header_capacity.get(),
                    4,
                    8_000,
                    retry_seconds,
                    peer_cache.clone(),
                ),
            )?
            .with_fee_policy(fee_policy);
            let limits = BlockDownloadConfig {
                max_attempts: 16,
                total_timeout: Duration::from_secs(total_seconds),
                visit_timeout: Duration::from_secs(network.timeout_seconds),
                max_received_bytes: 32 * 1024 * 1024,
                decode: BlockDecodeLimits {
                    max_payload_bytes: 4_000_000,
                    max_elements: 1_000_000,
                },
            };
            let block = tokio::select! {
                biased;
                result = &mut shutdown => { result?; sync.shutdown().await; return Err(interrupted()); }
                result = sync.fetch_block(height, hash, limits) => result?,
            };
            sync.shutdown().await;
            output::block(&path, &block)?;
            tracing::info!(height, %hash, output = %path.display(),
                "block written; protection unchanged");
        }
    }
    Ok(())
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
