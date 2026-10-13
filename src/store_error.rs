/// Errors from the storage boundary, not Bitcoin consensus rejection reasons.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The independently supplied guardian identity does not match this store.
    #[error("guardian binding mismatch; stop and reconcile operator records")]
    GuardianMismatch,
    /// Guardian commitment is not covered by a hash-verified local protection floor.
    #[error("guardian commitment is not proven protected; explicit coordination required")]
    UnprotectedCommitment,
    /// The requested height is older than this store's available history.
    #[error("height is outside retained history")]
    OutsideRetainedHistory,
    /// The requested height exceeds the current stored tip.
    #[error("height is beyond the stored tip")]
    BeyondTip,
    /// An update would require more retained ancestry or cross the anchor.
    #[error("update requires recovery outside retained ancestry")]
    RecoveryRequired,
    /// A caller supplied an inconsistent header or oversized update.
    #[error("invalid storage update: {0}")]
    InvalidUpdate(&'static str),
    /// Persisted configuration or identity differs from the caller's expectation.
    #[error("store configuration or anchor does not match")]
    ConfigurationMismatch,
    /// This library cannot read the application's disk format.
    #[error("unsupported header-store schema version: {0}")]
    UnsupportedSchema(u32),
    /// Persisted records violate the store's structural invariants.
    #[error("corrupt header store: {0}")]
    Corrupt(&'static str),
    /// Creating a new file failed; existing files are never overwritten.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// redb could not create or open the database.
    #[error(transparent)]
    Database(#[from] redb::DatabaseError),
    /// redb could not start a transaction.
    #[error(transparent)]
    Transaction(#[from] redb::TransactionError),
    /// redb could not access a table.
    #[error(transparent)]
    Table(#[from] redb::TableError),
    /// redb reported a storage failure.
    #[error(transparent)]
    Storage(#[from] redb::StorageError),
    /// redb could not select the required commit durability.
    #[error(transparent)]
    Durability(#[from] redb::SetDurabilityError),
    /// redb could not commit; drop and reopen the store before further use.
    #[error(transparent)]
    Commit(#[from] redb::CommitError),
}
