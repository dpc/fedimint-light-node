/// Fresh-bootstrap failures never authorize replacing an existing local checkpoint.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// Configuration violates trust, HTTPS, or resource bounds.
    #[error("invalid bootstrap configuration")]
    Configuration,
    /// The destination already exists, including a dangling symlink.
    #[error("bootstrap destination already exists")]
    AlreadyExists,
    /// A whole request or bootstrap exceeded its absolute deadline.
    #[error("bootstrap deadline exceeded")]
    Timeout,
    /// Operators returned different hashes at the selected common height.
    #[error("bootstrap sources disagree at the same height")]
    Disagreement,
    /// An endpoint did not serve the pinned mainnet identity.
    #[error("bootstrap source serves the wrong network")]
    WrongNetwork,
    /// The checkpoint is stale relative to the explicitly supplied local clock.
    #[error("bootstrap checkpoint is stale")]
    Stale,
    /// A source sent a malformed, non-success, oversized, or otherwise invalid reply.
    #[error("invalid bootstrap reply: {0}")]
    Reply(&'static str),
    /// Finite global resource budget exhausted.
    #[error("bootstrap resource limit: {0}")]
    Limit(&'static str),
    /// HTTP transport or TLS failed, including truncated replies.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// No configured peer completed authenticated history retrieval.
    #[error(transparent)]
    Peer(#[from] crate::sync_error::SyncError),
    /// Ancestry or checkpoint context failed authentication.
    #[error(transparent)]
    Header(#[from] crate::header_error::HeaderError),
    /// Local file operations failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
