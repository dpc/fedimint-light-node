use std::time::Duration;

use crate::block_decode::BlockDecodeLimits;

/// Explicit whole-operation budgets for a single-owner block retrieval.
///
/// There is one socket, payload and decoder globally per `HeaderSync`, shared
/// with header synchronization through exclusive borrowing, never per peer.
/// Returned blocks belong to the caller and no longer count against this budget.
#[derive(Debug, Clone, Copy)]
pub struct BlockDownloadConfig {
    /// One to sixteen attempts, capped further at the configured peer count.
    pub max_attempts: usize,
    /// Absolute deadline for all attempts and pacing, at most two minutes.
    pub total_timeout: Duration,
    /// Absolute connect/handshake/request deadline per peer, at most one minute.
    pub visit_timeout: Duration,
    /// Aggregate received wire bytes across handshakes, chatter and all attempts.
    ///
    /// From 24 bytes to 64 MB. Declared admitted payload bytes are reserved before
    /// reading, so a partial failed read conservatively consumes its entire length.
    pub max_received_bytes: usize,
    /// One payload up to 4 MB and up to one million aggregate decoded elements.
    pub decode: BlockDecodeLimits,
}

impl BlockDownloadConfig {
    /// Reject invalid or excessive budgets before connecting.
    pub(crate) fn validate(&self) -> bool {
        (1..=16).contains(&self.max_attempts)
            && !self.total_timeout.is_zero()
            && self.total_timeout <= Duration::from_secs(120)
            && !self.visit_timeout.is_zero()
            && self.visit_timeout <= Duration::from_secs(60)
            && (24..=64_000_000).contains(&self.max_received_bytes)
            && (1..=4_000_000).contains(&self.decode.max_payload_bytes)
            && (1..=1_000_000).contains(&self.decode.max_elements)
    }
}
