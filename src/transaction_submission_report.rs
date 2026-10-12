use std::net::SocketAddr;

use bitcoin::{Txid, Wtxid};

use crate::decode_error::DecodeError;
use crate::sync_error::SyncError;

/// Admission failure before any connection attempt or outbound buffer allocation.
#[derive(Debug, thiserror::Error)]
pub enum TransactionSubmissionError {
    /// Invalid or excessive resource policy.
    #[error("invalid transaction submission configuration")]
    Configuration,
    /// The exclusive owner has a terminal local failure.
    #[error("network owner stopped")]
    Stopped,
    /// Transaction exceeds byte/weight bounds or lacks inputs/outputs.
    #[error("transaction admission limit or empty inputs/outputs")]
    Admission,
    /// Raw input failed allocation-free canonical structural admission.
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

/// Observations from one finite submission operation, never peer acceptance.
#[derive(Debug)]
pub struct TransactionSubmissionReport {
    /// Legacy identifier of the admitted transaction.
    pub txid: Txid,
    /// Witness-inclusive identifier of the admitted transaction.
    pub wtxid: Wtxid,
    /// One entry for each selected peer, including interrupted pacing.
    pub attempts: Vec<SubmissionAttempt>,
    /// Conservatively reserved inbound wire bytes, including incomplete frames.
    pub received_bytes: usize,
    /// Conservatively reserved outbound wire bytes, including incomplete writes.
    pub sent_bytes: usize,
}

impl TransactionSubmissionReport {
    /// Count completed payload writes; zero means no observed submission.
    pub fn submitted(&self) -> usize {
        self.attempts.iter().filter(|a| a.result.is_ok()).count()
    }
}

/// A selected peer's write observation and failure, if any.
#[derive(Debug)]
pub struct SubmissionAttempt {
    /// Configured resolved peer address.
    pub peer: SocketAddr,
    /// True once the transaction write is about to begin; failure is indeterminate.
    pub payload_write_started: bool,
    /// Only a completed transaction frame write returns `Ok(())`.
    pub result: Result<(), SubmissionPeerError>,
    /// Best-effort post-write ping/pong drain, absent when no write completed.
    ///
    /// A matching pong is transport hygiene, not transaction acknowledgement.
    /// Failure here never erases a completed payload write.
    pub post_write: Option<Result<(), SubmissionPeerError>>,
}

/// Bounded peer failure, not a consensus or mempool rejection judgment.
#[derive(Debug, thiserror::Error)]
pub enum SubmissionPeerError {
    /// Absolute operation/visit deadline, including pacing.
    #[error("submission deadline")]
    Timeout,
    /// Aggregate outbound bytes could not admit the next whole frame.
    #[error("submission sent-byte budget")]
    SentBudget,
    /// Transport, inbound budget, handshake or peer capability failure.
    #[error(transparent)]
    Peer(#[from] SyncError),
}
