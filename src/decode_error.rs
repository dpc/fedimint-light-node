use crate::integrity_error::IntegrityError;

/// Rejection of an untrusted body, not invalidation of its requested header.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// Payload exceeds the caller or protocol byte limit.
    #[error("block payload exceeds byte limit")]
    PayloadLimit,
    /// Aggregate decoded collection entries exceed the caller's structural budget.
    #[error("block exceeds decoded element limit")]
    ElementLimit,
    /// A wire length cannot be represented on this host.
    #[error("block length exceeds host integer range")]
    LengthOverflow,
    /// A declared field or collection cannot fit in the available payload.
    #[error("truncated block payload")]
    Truncated,
    /// Data follows the complete block encoding.
    #[error("trailing block payload bytes")]
    TrailingBytes,
    /// The witness marker is present but no input has a witness stack.
    #[error("superfluous witness serialization")]
    SuperfluousWitness,
    /// The dependency rejected a malformed or noncanonical encoding.
    #[error("invalid block encoding: {0}")]
    Encoding(#[from] bitcoin::consensus::encode::Error),
    /// Decoding succeeded but mechanical integrity failed.
    #[error("block integrity failed: {0}")]
    Integrity(#[from] IntegrityError),
}
