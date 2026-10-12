/// Contextual header rejection or an explicit boundary requiring caller action.
#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    /// Only Bitcoin mainnet rules are implemented.
    #[error("contextual validation supports mainnet only")]
    UnsupportedNetwork,
    /// Bootstrap did not provide the bounded ancestry needed for validation.
    #[error("missing timestamp or retarget context")]
    MissingContext,
    /// Ancestry does not link to the expected parent or trusted anchor.
    #[error("header linkage or anchor mismatch")]
    Linkage,
    /// The declared target differs from the contextual expected compact target.
    #[error("unexpected difficulty target")]
    Difficulty,
    /// The timestamp is not strictly greater than median time past.
    #[error("timestamp is not greater than median time past")]
    MedianTime,
    /// The timestamp exceeds the caller's clock by more than two hours.
    #[error("timestamp is too far in the future")]
    FutureTime,
    /// A buried mainnet version rule rejects this header.
    #[error("obsolete block version")]
    Version,
    /// A target encoding, target range, or proof-of-work check failed.
    #[error(transparent)]
    ProofOfWork(#[from] crate::integrity_error::IntegrityError),
    /// Work comparisons require the identical trusted origin.
    #[error("relative work has different trusted origins")]
    DifferentAnchor,
    /// A fork crosses protected history or requires unavailable context.
    #[error("fork requires explicit recovery outside supported history")]
    RecoveryRequired,
    /// An alternative branch has no strictly greater relative work.
    #[error("candidate does not have more cumulative work")]
    InsufficientWork,
    /// Height or cumulative work cannot be represented without overflow.
    #[error("header height or cumulative work overflow")]
    Overflow,
    /// Persistent state could not be read or atomically updated.
    #[error(transparent)]
    Store(#[from] crate::store_error::StoreError),
}
