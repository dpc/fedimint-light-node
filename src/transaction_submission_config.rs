use std::time::Duration;

/// Finite per-call limits for outbound submission, not transaction validity policy.
#[derive(Debug, Clone, Copy)]
pub struct TransactionSubmissionConfig {
    /// Maximum canonical transaction bytes, from one through 400,000.
    pub max_transaction_bytes: usize,
    /// Maximum weight units, from one through 400,000 (not a standardness check).
    pub max_weight: u64,
    /// Sequential attempts, one through sixteen, also capped at configured peers.
    pub max_attempts: usize,
    /// All pacing and attempts, positive and no more than two minutes.
    pub total_timeout: Duration,
    /// Connect, handshake and write per peer, positive and at most one minute.
    pub visit_timeout: Duration,
    /// Aggregate received envelope/payload reservations, 24 through 64 MB.
    pub max_received_bytes: usize,
    /// Aggregate outbound frame reservations, 24 through 64 MB.
    ///
    /// Frames are charged before writing, including partial or failed writes.
    pub max_sent_bytes: usize,
}

impl Default for TransactionSubmissionConfig {
    fn default() -> Self {
        Self {
            max_transaction_bytes: 400_000,
            max_weight: 400_000,
            max_attempts: 3,
            total_timeout: Duration::from_secs(60),
            visit_timeout: Duration::from_secs(20),
            max_received_bytes: 2_000_000,
            max_sent_bytes: 1_210_000,
        }
    }
}

impl TransactionSubmissionConfig {
    /// Validate hard resource ceilings before allocation or networking.
    pub(crate) fn validate(&self) -> bool {
        (1..=400_000).contains(&self.max_transaction_bytes)
            && (1..=400_000).contains(&self.max_weight)
            && (1..=16).contains(&self.max_attempts)
            && !self.total_timeout.is_zero()
            && self.total_timeout <= Duration::from_secs(120)
            && !self.visit_timeout.is_zero()
            && self.visit_timeout <= Duration::from_secs(60)
            && (24..=64_000_000).contains(&self.max_received_bytes)
            && (24..=64_000_000).contains(&self.max_sent_bytes)
    }
}
