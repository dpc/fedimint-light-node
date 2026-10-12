use crate::decode_error::DecodeError;

/// Rejection of a v1 block frame, not invalidation of a retained header.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The caller did not supply exactly one fixed-size header.
    #[error("v1 header must contain exactly 24 bytes")]
    HeaderLength,
    /// Magic does not match the explicitly selected network.
    #[error("wrong v1 network magic")]
    Magic,
    /// Command is unsupported or not the canonical zero-padded block command.
    #[error("expected canonical v1 block command")]
    Command,
    /// Declared bytes exceed the caller, protocol or host length limit.
    #[error("v1 block payload exceeds byte limit")]
    PayloadLimit,
    /// Body is truncated or includes bytes beyond its declared length.
    #[error("v1 block payload length mismatch")]
    PayloadLength,
    /// The first four SHA256d bytes do not match the declared checksum.
    #[error("v1 block checksum mismatch")]
    Checksum,
    /// The bounded decoder or mechanical integrity checks rejected the block.
    #[error("v1 block admission failed: {0}")]
    Block(#[from] DecodeError),
}
