use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

use bitcoin::BlockHash;
use clap::{Args, Parser, Subcommand};
use fedimint_light_node::bootstrap_config::BootstrapSource;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::sync_config::SyncConfig;

/// Thin mainnet operations; selected headers never prove freshness or consensus.
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Options {
    /// Database path in a trusted local directory; never implicitly created on open.
    #[arg(long)]
    pub db: PathBuf,
    /// Persisted header capacity; must match on every reopen (no resizing).
    #[arg(long, global = true, default_value_t = 52_560, value_parser = clap::value_parser!(u64).range(1..=105_120))]
    pub capacity: u64,
    /// Database cache in MiB, not an RSS or disk-size limit.
    #[arg(long, global = true, default_value_t = 8, value_parser = clap::value_parser!(u64).range(1..=64))]
    pub cache_mib: u64,
    /// Requested operation.
    #[command(subcommand)]
    pub command: Command,
}

impl Options {
    /// Convert checked CLI budgets to the library's explicit storage configuration.
    pub fn store_config(&self) -> StoreConfig {
        StoreConfig {
            header_capacity: NonZeroU64::new(self.capacity).expect("checked by clap"),
            cache_bytes: NonZeroUsize::new(self.cache_mib as usize * 1024 * 1024)
                .expect("checked by clap"),
        }
    }
}

/// Operations remain explicit; opening never falls back to bootstrap.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a NEW store using trusted HTTPS agreement and P2P ancestry.
    Bootstrap {
        #[command(flatten)]
        network: Network,
        /// Replace default sources with repeated OPERATOR=HTTPS_API_ROOT entries.
        #[arg(long, value_parser = parse_source)]
        source: Vec<BootstrapSource>,
        /// Successful independent operators required (also a strict majority).
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(2..=8))]
        quorum: u64,
        /// Successor blocks below the lower reported source tip, not wallet finality.
        #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u64).range(0..=2016))]
        confirmation_margin: u64,
        /// Total asynchronous bootstrap deadline in seconds.
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=3600))]
        total_seconds: u64,
    },
    /// Report pinned and selected state offline, not node health.
    Status,
    /// Visit configured peers repeatedly until interrupted; --once visits one peer.
    Sync {
        #[command(flatten)]
        network: Network,
        /// Delay before every connection attempt, including unsuccessful attempts.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=3600))]
        retry_seconds: u64,
        /// Perform one bounded visit and return failure if that visit fails.
        #[arg(long)]
        once: bool,
        /// Header requests per visit before rotating to the next peer.
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u64).range(1..=16))]
        requests: u64,
        /// Maximum candidate suffix (also capped at the configured capacity).
        #[arg(long, default_value_t = 8_000, value_parser = clap::value_parser!(u64).range(1..=32_000))]
        candidate_headers: u64,
    },
    /// Retrieve a retained selected block as canonical witness-bearing block bytes.
    Fetch {
        #[command(flatten)]
        network: Network,
        /// Delay before every connection attempt, included in the total deadline.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=3600))]
        retry_seconds: u64,
        /// Retained selected height; outside-history requests fail without recovery.
        #[arg(long)]
        height: u64,
        /// Optional expected selected hash, checked before requesting a body.
        #[arg(long)]
        hash: Option<BlockHash>,
        /// NEW output file in a trusted directory; existing files/symlinks are refused.
        #[arg(long)]
        output: PathBuf,
        /// Total request deadline including pacing and all peer attempts.
        #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=120))]
        total_seconds: u64,
    },
    /// Write a transaction to up to three peers; no acceptance/confirmation guarantee.
    Broadcast {
        #[command(flatten)]
        network: Network,
        /// Canonical binary transaction in a regular file in a trusted directory.
        #[arg(long)]
        input: PathBuf,
        /// Delay before every attempt, included in the total deadline.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=3600))]
        retry_seconds: u64,
        /// Total pacing and submission deadline, not an inactivity timeout.
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=120))]
        total_seconds: u64,
    },
    /// Persist a monotonic rollback floor for consumed history, offline.
    Protect {
        /// Retained consumed height; this cannot undo an earlier protection choice.
        #[arg(long)]
        height: u64,
        /// Expected selected hash, required to avoid protecting a changed selection.
        #[arg(long)]
        hash: BlockHash,
    },
}

/// Explicit resolved peers; there is no discovery or P2P DNS in the executable.
#[derive(Debug, Args)]
pub struct Network {
    /// Repeat for 1..16 IPv4:port or [IPv6]:port addresses, visited sequentially.
    #[arg(long, required = true)]
    pub peer: Vec<SocketAddr>,
    /// Whole exchange deadline in seconds, not an inactivity timeout.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=60))]
    pub timeout_seconds: u64,
}

impl Network {
    /// Construct the bounded, sequential service configuration.
    pub fn sync_config(
        &self,
        capacity: u64,
        requests: u64,
        candidates: u64,
        retry_seconds: u64,
    ) -> SyncConfig {
        SyncConfig {
            peers: self.peer.clone(),
            handshake_timeout: Duration::from_secs(self.timeout_seconds),
            request_timeout: Duration::from_secs(self.timeout_seconds),
            retry_delay: Duration::from_secs(retry_seconds),
            requests_per_visit: requests as usize,
            candidate_headers: candidates.min(capacity) as usize,
        }
    }
}

fn parse_source(value: &str) -> Result<BootstrapSource, String> {
    let (operator, api) = value
        .split_once('=')
        .ok_or("expected OPERATOR=HTTPS_API_ROOT")?;
    if operator.is_empty() || operator.len() > 128 || api.len() > 2048 {
        return Err("invalid operator or source length".into());
    }
    let url = reqwest::Url::parse(api).map_err(|e| e.to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("source must be HTTPS without credentials, query or fragment".into());
    }
    Ok(BootstrapSource {
        operator: operator.into(),
        api: api.into(),
    })
}
