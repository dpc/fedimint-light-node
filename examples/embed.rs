//! A caller-owned lifecycle, not a guardian adapter or an automatic fallback.
//!
//! Opens an existing pinned store offline, visits one peer, then requests a retained
//! body. The caller explicitly opts into protecting the returned selection.

use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use fedimint_light_node::block_decode::BlockDecodeLimits;
use fedimint_light_node::block_download_config::BlockDownloadConfig;
use fedimint_light_node::block_download_error::BlockDownloadError;
use fedimint_light_node::bootstrap_config::BootstrapConfig;
use fedimint_light_node::header_sync::HeaderSync;
use fedimint_light_node::sync_config::SyncConfig;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;

/// Demonstrate embedding without global signals, tracing, tasks or listeners.
#[derive(Parser)]
struct Args {
    /// Existing database created by bootstrap.
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
    // No network, oracle lookup, implicit creation or re-anchoring on reopen.
    let store = ValidatedHeaderStore::open_pinned(&args.db, storage)?;
    let mut node = HeaderSync::new(
        store,
        SyncConfig {
            peers: vec![args.peer],
            handshake_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(10),
            retry_delay: Duration::from_secs(1),
            requests_per_visit: 2,
            candidate_headers: args.capacity.min(8_000) as usize,
        },
    )?;

    // The application supplies this future: it could instead be a channel,
    // cancellation token or host shutdown notification. It spans both operations.
    let stop = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(stop);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let report = tokio::select! {
        biased;
        _ = &mut stop => {
            eprintln!("canceled; selected state may include committed progress: {:?}", node.status()?);
            return Ok(());
        }
        report = node.step(now) => report,
    };
    // A retrying host may continue on nonterminal peer errors. This small example
    // returns all failures instead of hiding them or treating a stored tip as health.
    report.result?;
    let selected = node.status()?;
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
        result = node.fetch_block(selected.height, selected.hash, fetch) => result,
    };
    let block = match result {
        Ok(block) => block,
        Err(BlockDownloadError::OutsideRetainedHistory) => {
            // Permanent: the host must stop or explicitly choose another backend,
            // not blindly retry this old height forever.
            return Err("requested height is no longer retained".into());
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
    drop(node.into_store());
    Ok(())
}
