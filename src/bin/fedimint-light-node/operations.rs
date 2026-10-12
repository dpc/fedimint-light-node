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

use crate::options::{Command, Options};
use crate::{Result, interrupted, output};

/// Dispatch explicit operations without adding chain logic or implicit recovery.
pub async fn run(
    options: Options,
    mut shutdown: Pin<&mut impl Future<Output = io::Result<()>>>,
) -> Result<()> {
    let config = options.store_config();
    let fee_policy = options.fee_policy();
    let mut peer_cache = options.db.as_os_str().to_os_string();
    peer_cache.push(".peers");
    let peer_cache = std::path::PathBuf::from(peer_cache);
    output::fee_policy(fee_policy)?;
    match options.command {
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
            eprintln!(
                "bootstrap: trusting configured source agreement; no consensus/freshness guarantee"
            );
            let store = tokio::select! {
                biased;
                result = &mut shutdown => { result?; return Err(interrupted()); }
                result = bootstrap(&options.db, config, now()?) => result?,
            };
            output::status(&store)?;
        }
        Command::Status => {
            output::status(&ValidatedHeaderStore::open_pinned(&options.db, config)?)?;
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
            let store = ValidatedHeaderStore::open_pinned(&options.db, config)?;
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
            eprintln!(
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
            let mut store = ValidatedHeaderStore::open_pinned(&options.db, config)?;
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
            let store = ValidatedHeaderStore::open_pinned(&options.db, config)?;
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
                let status = sync.status()?;
                eprintln!(
                    "peer={:?} selected_height={} selected_hash={} generation={} stopped={}",
                    report.peer, status.height, status.hash, status.generation, status.stopped,
                );
                let (connections, candidates, cache_failed) = sync.peer_counts();
                eprintln!(
                    "connections={connections} candidates={candidates} peer_cache_write_failed={cache_failed}"
                );
                match report.result {
                    Ok(end) => eprintln!("visit={end:?}; freshness remains unproven"),
                    Err(error) => {
                        eprintln!("visit failed: {error}");
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
            let store = ValidatedHeaderStore::open_pinned(&options.db, config)?;
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
            eprintln!(
                "wrote height={height} hash={hash} to {}; protection unchanged",
                path.display()
            );
        }
    }
    Ok(())
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
