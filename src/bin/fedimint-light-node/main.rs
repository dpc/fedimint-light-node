//! Runtime, signals and operational presentation belong to this thin executable.

mod operations;
mod options;
mod output;
#[cfg(test)]
mod output_tests;
mod rpc_credentials;
mod transaction_input;

use std::future::Future;
use std::io;
use std::process::ExitCode;

use clap::Parser;
use options::Options;

/// Executable errors preserve underlying library diagnostics.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let options = Options::parse();
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::Interrupted)
            {
                ExitCode::from(130)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn run(options: Options) -> Result<()> {
    // No runtime or global signal/logging configuration is imposed by the library.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // Register before starting work. Direct P2P futures are dropped on shutdown;
        // synchronous DB work and HTTP OS resolver jobs cannot be forcibly stopped.
        let shutdown = shutdown()?;
        tokio::pin!(shutdown);
        operations::run(options, shutdown).await
    })
}

#[cfg(unix)]
fn shutdown() -> io::Result<impl Future<Output = io::Result<()>>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        Ok(())
    })
}

#[cfg(not(unix))]
fn shutdown() -> io::Result<impl Future<Output = io::Result<()>>> {
    Ok(tokio::signal::ctrl_c())
}

/// Cancellation is explicit and non-successful; committed batches remain on disk.
fn interrupted() -> Box<dyn std::error::Error> {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "interrupted; inspect stored state before resuming",
    )
    .into()
}
