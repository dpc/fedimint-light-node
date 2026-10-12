use std::num::{NonZeroU64, NonZeroUsize};

/// Explicit resource settings for a header store, with no production defaults.
#[derive(Clone, Copy, Debug)]
pub struct StoreConfig {
    /// Maximum number of queryable canonical headers, persisted at creation.
    ///
    /// Reopening requires the same capacity; resizing is not implemented.
    pub header_capacity: NonZeroU64,
    /// redb's cache budget in bytes, supplied again on every open.
    ///
    /// This is not a process RSS or physical database-file size limit.
    pub cache_bytes: NonZeroUsize,
}
