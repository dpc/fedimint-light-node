use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use crate::store_config::StoreConfig;

/// A caller-attested independent operator exposing the Esplora API over HTTPS.
///
/// Different hostnames do not prove independent control. Configuration is an
/// explicit trust decision, not a discovery mechanism or consensus oracle.
#[derive(Clone, Debug)]
pub struct BootstrapSource {
    /// Operator identity; duplicate identities cannot contribute extra votes.
    pub operator: String,
    /// HTTPS API root, without credentials, query, or fragment.
    pub api: String,
}

/// Finite budgets and explicit trust policy for one fresh mainnet initialization.
#[derive(Clone, Debug)]
pub struct BootstrapConfig {
    /// Two to eight independently operated sources.
    pub sources: Vec<BootstrapSource>,
    /// Required successful agreeing operators; at least two and a strict majority.
    /// Any observed hash disagreement fails closed, even with a sufficient majority.
    pub quorum: usize,
    /// Select the minimum successfully reported tip minus this many blocks.
    pub confirmation_margin: u64,
    /// Reject checkpoints older than this duration relative to the supplied clock.
    pub max_checkpoint_age: Duration,
    /// Per HTTP request and P2P exchange absolute deadline, including body reads.
    pub request_timeout: Duration,
    /// Total asynchronous bootstrap deadline across all sources and peers.
    pub total_timeout: Duration,
    /// Up to sixteen explicit addresses; discovery can initialize an empty list.
    pub peers: Vec<SocketAddr>,
    /// Bounded maintained mainnet connections and discovery policy.
    pub peer_pool: crate::peer_pool_config::PeerPoolConfig,
    /// Global number of getheaders requests across peers, not per peer.
    pub max_header_requests: usize,
    /// Global received P2P byte budget across handshakes, chatter and headers.
    pub max_peer_bytes: usize,
    /// Rolling header capacity and database cache budget.
    pub store: StoreConfig,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            sources: vec![
                BootstrapSource {
                    operator: "Blockstream".into(),
                    api: "https://blockstream.info/api".into(),
                },
                BootstrapSource {
                    operator: "mempool.space".into(),
                    api: "https://mempool.space/api".into(),
                },
            ],
            quorum: 2,
            confirmation_margin: 6,
            max_checkpoint_age: Duration::from_secs(48 * 3600),
            request_timeout: Duration::from_secs(20),
            total_timeout: Duration::from_secs(600),
            peers: Vec::new(),
            peer_pool: crate::peer_pool_config::PeerPoolConfig::default(),
            max_header_requests: 128,
            max_peer_bytes: 32 * 1024 * 1024,
            store: StoreConfig {
                header_capacity: NonZeroU64::new(52_560).expect("nonzero"),
                cache_bytes: NonZeroUsize::new(8 * 1024 * 1024).expect("nonzero"),
            },
        }
    }
}

impl BootstrapConfig {
    /// Bound configuration before allocation, networking, or database creation.
    pub(crate) fn valid(&self) -> bool {
        (2..=8).contains(&self.sources.len())
            && self.quorum >= 2
            && self.quorum > self.sources.len() / 2
            && self.quorum <= self.sources.len()
            && self
                .sources
                .iter()
                .all(|s| !s.operator.is_empty() && s.operator.len() <= 128 && s.api.len() <= 2048)
            && self.sources.iter().enumerate().all(|(i, s)| {
                self.sources[..i]
                    .iter()
                    .all(|other| other.operator != s.operator)
            })
            && self.confirmation_margin <= 2016
            && !self.max_checkpoint_age.is_zero()
            && self.max_checkpoint_age <= Duration::from_secs(7 * 86400)
            && !self.request_timeout.is_zero()
            && self.request_timeout <= Duration::from_secs(60)
            && !self.total_timeout.is_zero()
            && self.total_timeout <= Duration::from_secs(3600)
            && self.peers.len() <= 16
            && (self.peer_pool.discovery || !self.peers.is_empty())
            && self.peer_pool.valid()
            && (1..=2048).contains(&self.max_header_requests)
            && (162_027..=256 * 1024 * 1024).contains(&self.max_peer_bytes)
            && self.store.header_capacity.get() <= 105_120
            && self.store.cache_bytes.get() <= 64 * 1024 * 1024
    }
}
