use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

use bitcoin::BlockHash;
use clap::{Args, Parser, Subcommand};
use fedimint_light_node::bootstrap_config::BootstrapSource;
use fedimint_light_node::fee_policy::FeePolicy;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::sync_config::SyncConfig;

/// Thin mainnet operations; selected headers never prove freshness or consensus.
#[derive(Clone, Debug, Parser)]
#[command(
    version,
    about = "Mainnet light node: with no command, open or bootstrap persistent state, sync and serve read-only localhost RPC. Not a full consensus validator."
)]
pub struct Options {
    /// Database path; normal startup defaults to $XDG_STATE_HOME/fedimint-light-node/chain.redb.
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Persisted header capacity; must match on every reopen (no resizing).
    #[arg(long, global = true, default_value_t = 52_560, value_parser = clap::value_parser!(u64).range(1..=105_120))]
    pub capacity: u64,
    /// Database cache in MiB, not an RSS or disk-size limit.
    #[arg(long, global = true, default_value_t = 8, value_parser = clap::value_parser!(u64).range(1..=64))]
    pub cache_mib: u64,
    /// Positive operator-supplied sats per 1,000 virtual bytes; omitted = unavailable.
    /// Static policy only, never a network estimate; not persisted in the database.
    #[arg(long, global = true)]
    pub fixed_fee_sats_per_kvb: Option<NonZeroU64>,
    /// Independently recorded 32-byte guardian database identity in hex (not a secret).
    #[arg(long, global = true)]
    pub guardian_id: Option<bitcoin::hashes::sha256::Hash>,
    /// Normal startup network options; for explicit commands put these after the command.
    #[command(flatten)]
    pub network: Network,
    /// Normal startup RPC options; omitted credentials mean read-only unauthenticated RPC.
    #[command(flatten)]
    pub rpc: RpcOptions,
    /// Requested operation; omitted = open or bootstrap state, sync and serve local reads.
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Options {
    /// Reject root-level service settings rather than silently ignoring them with a command.
    pub fn check_command_scope(&self) -> crate::Result<()> {
        if self.command.is_some()
            && (self.rpc.rpc_credentials.is_some()
                || self.rpc.rpc_no_auth
                || self.rpc.fake_bitcoind
                || self.rpc.mempool
                || self.rpc.allow_rpc_submission
                || self.rpc.rpc_bind
                    != "127.0.0.1:18443"
                        .parse::<SocketAddr>()
                        .expect("constant address")
                || self.network.manual_only
                || !self.network.peer.is_empty()
                || self.network.timeout_seconds != 20)
        {
            return Err("put network and RPC options after the explicit command, or omit the command for normal startup".into());
        }
        Ok(())
    }
    /// Resolve explicit caller fee configuration without inventing an estimate.
    pub fn fee_policy(&self) -> FeePolicy {
        self.fixed_fee_sats_per_kvb
            .map(|sats_per_kvb| FeePolicy::Fixed { sats_per_kvb })
            .unwrap_or_default()
    }
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
#[derive(Clone, Debug, Subcommand)]
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
    /// Write an offline safety-state record to NEW output; keep independently of backups.
    Checkpoint {
        /// New JSON file in a trusted directory.
        #[arg(long)]
        output: PathBuf,
    },
    /// Bind permanently to a guardian after verifying its independently recorded state.
    BindGuardian {
        #[command(flatten)]
        guardian: GuardianState,
    },
    /// Logical stopped-store snapshot plus a hash-checked manifest; no hot file copies.
    Backup {
        /// New database snapshot file.
        #[arg(long)]
        output: PathBuf,
        /// New JSON manifest, published last.
        #[arg(long)]
        manifest: PathBuf,
    },
    /// Restore only matching independently retained CURRENT safety state into a NEW db.
    Restore {
        /// Stopped snapshot database, never modified in place.
        #[arg(long)]
        backup: PathBuf,
        /// Snapshot integrity and identity manifest.
        #[arg(long)]
        manifest: PathBuf,
        /// Independent current checkpoint, NOT a copy derived from this backup.
        #[arg(long)]
        expected_state: PathBuf,
    },
    /// Sync while serving bounded RPC on loopback (authenticated unless --rpc-no-auth).
    Serve {
        #[command(flatten)]
        network: Network,
        #[command(flatten)]
        rpc: RpcOptions,
        /// Explicit unbound service; never a guardian-finality integration.
        #[arg(long)]
        standalone: bool,
        /// Bound guardian startup evidence, required unless --standalone.
        #[command(flatten)]
        guardian: GuardianState,
    },
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

/// RPC flags shared by normal startup and explicit serving.
#[derive(Clone, Debug, Args)]
pub struct RpcOptions {
    /// Observe an approximate unvalidated mempool and download recent selected bodies.
    #[arg(long)]
    pub mempool: bool,
    /// Numeric loopback listen address; use port 0 to select an available port.
    #[arg(long, default_value = "127.0.0.1:18443")]
    pub rpc_bind: SocketAddr,
    /// Private regular file containing username:password (optional final newline).
    #[arg(long, conflicts_with = "rpc_no_auth")]
    pub rpc_credentials: Option<PathBuf>,
    /// Allow read-only local RPC without credentials (normal startup default).
    #[arg(long, conflicts_with = "allow_rpc_submission")]
    pub rpc_no_auth: bool,
    /// Opt in to synthetic Core status, NOT validation, freshness or readiness.
    #[arg(long)]
    pub fake_bitcoind: bool,
    /// Enable submission-only sendrawtransaction WITHOUT Core fee/burn safeguards.
    #[arg(long, requires = "rpc_credentials")]
    pub allow_rpc_submission: bool,
}

/// Explicit guardian watermark; not a claim inferred from network or RPC activity.
#[derive(Clone, Debug, Default, Args)]
pub struct GuardianState {
    /// Last irreversibly consumed block HEIGHT (not Fedimint block count).
    #[arg(long)]
    pub committed_height: Option<u64>,
    /// Independently recorded hash at committed-height.
    #[arg(long)]
    pub committed_hash: Option<BlockHash>,
    /// Oldest block the guardian still needs; tip+1 is allowed when caught up.
    #[arg(long)]
    pub oldest_needed: Option<u64>,
}

impl GuardianState {
    /// Reject partial or absent state rather than manufacturing a consumer watermark.
    pub fn preflight(
        &self,
        id: Option<bitcoin::hashes::sha256::Hash>,
    ) -> crate::Result<fedimint_light_node::guardian_binding::GuardianPreflight> {
        use bitcoin::hashes::Hash;
        Ok(fedimint_light_node::guardian_binding::GuardianPreflight {
            guardian_id: id.ok_or("--guardian-id is required")?.to_byte_array(),
            committed_height: self
                .committed_height
                .ok_or("--committed-height is required")?,
            committed_hash: self.committed_hash.ok_or("--committed-hash is required")?,
            oldest_needed: self.oldest_needed.ok_or("--oldest-needed is required")?,
        })
    }

    /// Determine whether any caller watermark was supplied.
    pub fn is_present(&self) -> bool {
        self.committed_height.is_some()
            || self.committed_hash.is_some()
            || self.oldest_needed.is_some()
    }
}

/// Public discovery by default, supplemented or replaced by explicit numeric peers.
#[derive(Clone, Debug, Args)]
pub struct Network {
    /// Optional IPv4:port or [IPv6]:port peers; repeat up to sixteen times.
    #[arg(long)]
    pub peer: Vec<SocketAddr>,
    /// Disable DNS/gossip dialing; use only explicitly configured peers (including private nodes).
    #[arg(long)]
    pub manual_only: bool,
    /// Whole exchange deadline in seconds, not an inactivity timeout.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=60))]
    pub timeout_seconds: u64,
}

impl Network {
    /// Construct the bounded foreground service and maintained pool configuration.
    pub fn sync_config(
        &self,
        capacity: u64,
        requests: u64,
        candidates: u64,
        retry_seconds: u64,
        cache: PathBuf,
    ) -> SyncConfig {
        SyncConfig {
            peer_pool: {
                let mut pool = self.pool_config();
                pool.cache = Some(cache);
                pool
            },
            peers: self.peer.clone(),
            handshake_timeout: Duration::from_secs(self.timeout_seconds),
            request_timeout: Duration::from_secs(self.timeout_seconds),
            retry_delay: Duration::from_secs(retry_seconds),
            requests_per_visit: requests as usize,
            candidate_headers: candidates.min(capacity) as usize,
        }
    }

    /// Use public discovery by default, with an explicit private/manual-only override.
    pub fn pool_config(&self) -> fedimint_light_node::peer_pool_config::PeerPoolConfig {
        if self.manual_only {
            fedimint_light_node::peer_pool_config::PeerPoolConfig::manual_only()
        } else {
            fedimint_light_node::peer_pool_config::PeerPoolConfig::default()
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
