//! A caller-owned lifecycle, not a guardian adapter or an automatic fallback.
//!
//! Explicit creation or offline reopen, queries, one peer visit, body retrieval,
//! optional protection and bounded submission. No consumer database is implemented.

use std::error::Error;
use std::io::Read;
use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use fedimint_light_node::block_decode::BlockDecodeLimits;
use fedimint_light_node::block_download_config::BlockDownloadConfig;
use fedimint_light_node::block_download_error::BlockDownloadError;
use fedimint_light_node::bootstrap::bootstrap;
use fedimint_light_node::bootstrap_config::BootstrapConfig;
use fedimint_light_node::fee_policy::FeePolicy;
use fedimint_light_node::header_sync::HeaderSync;
use fedimint_light_node::sync_config::SyncConfig;
use fedimint_light_node::transaction_decode::decode_transaction;
use fedimint_light_node::transaction_submission_config::TransactionSubmissionConfig;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;

/// Demonstrate embedding without global signals, tracing, tasks or listeners.
#[derive(Parser)]
struct Args {
    /// Database in a caller-owned trusted directory.
    #[arg(long)]
    db: PathBuf,
    /// Caller-resolved witness-serving full-history peer.
    #[arg(long)]
    peer: SocketAddr,
    /// Must equal the capacity used when creating the store.
    #[arg(long, default_value_t = 52_560, value_parser = clap::value_parser!(u64).range(1..=105_120))]
    capacity: u64,
    /// Explicitly pin the returned selection; this cannot be undone.
    #[arg(long)]
    protect: bool,
    /// Explicitly create a NEW store with the default trusted HTTPS source policy.
    #[arg(long)]
    bootstrap: bool,
    /// Positive operator-supplied sats per 1,000 virtual bytes, not an estimate.
    #[arg(long)]
    fixed_fee_sats_per_kvb: Option<NonZeroU64>,
    /// Optionally submit this caller-owned canonical raw transaction file.
    #[arg(long)]
    transaction: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    // An application may reuse its runtime instead. A dedicated caller-owned
    // runtime thread keeps synchronous DB work off latency-sensitive executors.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(args))
}

async fn run(args: Args) -> Result<(), Box<dyn Error>> {
    let mut storage = BootstrapConfig::default().store;
    storage.header_capacity = args.capacity.try_into()?;
    // Replace this caller-owned deadline with a host cancellation future. It spans
    // the entire lifecycle; cancellation cannot interrupt synchronous DB work.
    let stop = tokio::time::sleep(Duration::from_secs(600));
    tokio::pin!(stop);
    let store = if args.bootstrap {
        let config = BootstrapConfig {
            peer_pool: fedimint_light_node::peer_pool_config::PeerPoolConfig::manual_only(),
            store: storage,
            peers: vec![args.peer],
            ..BootstrapConfig::default()
        };
        // HTTPS source agreement is explicitly trusted, not consensus. Resolver
        // jobs can outlive cancellation; publication errors require inspection.
        tokio::select! {
            biased;
            _ = &mut stop => return Ok(()),
            result = bootstrap(&args.db, config, now()?) => result?,
        }
    } else {
        // No network, implicit creation, oracle lookup or re-anchoring on reopen.
        ValidatedHeaderStore::open_pinned(&args.db, storage)?
    };
    let fee_policy = args
        .fixed_fee_sats_per_kvb
        .map(|sats_per_kvb| FeePolicy::Fixed { sats_per_kvb })
        .unwrap_or_default();
    let mut node = HeaderSync::new(
        store,
        SyncConfig {
            peer_pool: fedimint_light_node::peer_pool_config::PeerPoolConfig::manual_only(),
            peers: vec![args.peer],
            handshake_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(10),
            retry_delay: Duration::from_secs(1),
            requests_per_visit: 2,
            candidate_headers: args.capacity.min(8_000) as usize,
        },
    )?
    .with_fee_policy(fee_policy);
    println!(
        "selected_count={} height_one_hash={} operator_fee_policy={:?} sync_progress={:?}; no fee estimate; freshness unproven",
        node.block_count()?,
        node.chain_id(),
        node.fee_policy(),
        node.sync_progress(),
    );

    let report = tokio::select! {
        biased;
        _ = &mut stop => {
            eprintln!("canceled; selected state may include committed progress: {:?}", node.status()?);
            return Ok(());
        }
        report = node.step(now()?) => report,
    };
    // A retrying host may continue on nonterminal peer errors. This small example
    // returns all failures instead of hiding them or treating a stored tip as health.
    report.result?;
    let selected = node.status()?;
    let hash = node.block_hash(selected.height)?;
    let fetch = BlockDownloadConfig {
        max_attempts: 1,
        total_timeout: Duration::from_secs(30),
        visit_timeout: Duration::from_secs(20),
        max_received_bytes: 8 * 1024 * 1024,
        decode: BlockDecodeLimits {
            max_payload_bytes: 4_000_000,
            max_elements: 1_000_000,
        },
    };
    let result = tokio::select! {
        biased;
        _ = &mut stop => return Ok(()),
        result = node.fetch_block(selected.height, hash, fetch) => result,
    };
    let block = match result {
        Ok(block) => block,
        Err(error @ BlockDownloadError::OutsideRetainedHistory) => {
            // Permanent: the host must stop or explicitly choose another backend,
            // not blindly retry this old height forever.
            return Err(error.into());
        }
        Err(error) => return Err(error.into()),
    };
    if args.protect {
        // No step/selection occurred between delivery and protection. Persist this
        // floor before exposing irreversible consumer state. A real application
        // must design crash ordering with its own durable consumer transactions.
        node.protect_through(selected.height, selected.hash)?;
    }
    println!(
        "height={} hash={} transactions={} protected={}",
        selected.height,
        block.block_hash(),
        block.txdata.len(),
        args.protect,
    );
    // Dropping the caller-owned block releases its allocations. No body cache or
    // maintenance task remains; into_store hands persistence ownership back.
    drop(block);
    if let Some(path) = args.transaction {
        let limits = TransactionSubmissionConfig::default();
        // The parent directory is trusted. Reject special files and bound the read
        // even if a regular file grows. The bytes and decoded transaction are ours.
        if !std::fs::symlink_metadata(&path)?.is_file() {
            return Err("transaction must be a regular file".into());
        }
        let file = std::fs::File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err("transaction must be a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(limits.max_transaction_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        let transaction = decode_transaction(&bytes, limits)?;
        drop(bytes);
        let report = tokio::select! {
            biased;
            _ = &mut stop => {
                eprintln!("submission canceled: delivery is indeterminate; caller decides retry");
                return Ok(());
            }
            result = node.submit_transaction(&transaction, limits) => result?,
        };
        println!(
            "completed local writes={}; acceptance/propagation/confirmation unproven",
            report.submitted()
        );
        if report.submitted() == 0 {
            return Err("no completed submission; started writes may still have delivered".into());
        }
    }
    drop(node.into_store());
    // Reopen offline to observe durable protection. Fee policy is NOT persisted:
    // a new HeaderSync defaults to Unavailable unless the host configures it again.
    let reopened = ValidatedHeaderStore::open_pinned(&args.db, storage)?;
    println!("reopened protected_height={}", reopened.protected_height()?);
    Ok(())
}

fn now() -> Result<u64, Box<dyn Error>> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
