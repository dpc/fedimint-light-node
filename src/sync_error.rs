use crate::header_error::HeaderError;
use crate::store_error::StoreError;

/// A bounded peer failure or a terminal local/recovery error.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// Caller budgets are invalid.
    #[error("invalid synchronization configuration")]
    Configuration,
    /// An absolute handshake or request deadline expired.
    #[error("peer deadline expired")]
    Timeout,
    /// Peer TCP transport failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A peer violated the supported bounded protocol.
    #[error("peer protocol violation: {0}")]
    Protocol(&'static str),
    /// Remote ancestry or a fork exceeds available context, protection or capacity.
    ///
    /// This is not evidence that selected state needs recovery. Stop this peer
    /// visit and rotate; an untrusted assertion must not stop all synchronization.
    #[error("unsupported remote candidate")]
    UnsupportedCandidate,
    /// The service has latched a previous terminal error.
    #[error("synchronization stopped; explicit recovery required")]
    Stopped,
    /// Contextual header admission failed.
    #[error(transparent)]
    Header(#[from] HeaderError),
    /// Local persistent state failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl SyncError {
    /// Distinguish retryable peer rejection from failures that stop this service.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Configuration
                | Self::Stopped
                | Self::Store(_)
                | Self::Header(
                    HeaderError::RecoveryRequired
                        | HeaderError::Store(_)
                        | HeaderError::DifferentAnchor
                        | HeaderError::Overflow
                )
        )
    }
}
