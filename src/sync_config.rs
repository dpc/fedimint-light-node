use std::net::SocketAddr;
use std::time::Duration;

/// Explicit budgets for a single-foreground, outbound-only mainnet synchronizer.
///
/// There is exactly one foreground request and candidate globally per service,
/// bounded idle drivers, and no persistent alternative branches. Hard ceilings
/// bound misconfiguration too; these are implementation limits, not production defaults.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Up to sixteen explicit numeric addresses; empty is valid with discovery enabled.
    pub peers: Vec<SocketAddr>,
    /// Discovery, stable/exploratory sockets and optional successful-peer cache.
    pub peer_pool: crate::peer_pool_config::PeerPoolConfig,
    /// Whole connect plus version/verack exchange deadline (at most one minute).
    pub handshake_timeout: Duration,
    /// Whole getheaders exchange deadline, including chatter (at most one minute).
    pub request_timeout: Duration,
    /// Minimum delay before each connection attempt (at most one hour, nonzero).
    pub retry_delay: Duration,
    /// One to sixteen requests per peer visit, enforcing fair rotation.
    pub requests_per_visit: usize,
    /// Maximum unselected candidate length, one to 32,000 and no greater than storage capacity.
    pub candidate_headers: usize,
}

impl SyncConfig {
    /// Reject zero, excessive, or inconsistent budgets before any network activity.
    pub(crate) fn validate(&self, capacity: u64) -> bool {
        self.peers.len() <= 16
            && (self.peer_pool.discovery || !self.peers.is_empty())
            && self.peer_pool.valid()
            && (1..=16).contains(&self.requests_per_visit)
            && (1..=32_000).contains(&self.candidate_headers)
            && self.candidate_headers as u64 <= capacity
            && [self.handshake_timeout, self.request_timeout]
                .iter()
                .all(|d| !d.is_zero() && *d <= Duration::from_secs(60))
            && !self.retry_delay.is_zero()
            && self.retry_delay <= Duration::from_secs(3600)
    }
}
