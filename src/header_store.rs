use std::fs::OpenOptions;
use std::path::Path;

use bitcoin::block::Header;
use bitcoin::hashes::Hash;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};

use crate::header_context::HeaderContext;
use crate::retained_range::RetainedRange;
use crate::store_anchor::StoreAnchor;
use crate::store_config::StoreConfig;
use crate::store_error::StoreError;
use crate::store_format::{CONTEXTS, HEADERS, META, Metadata, decode_header, encode_header};

/// Synchronous, bounded canonical-header persistence for an embedding caller.
///
/// **Not a validator:** the caller must authenticate the anchor, validate every
/// header, and select a branch before storing it (SPEC-chain-trust). Link checks
/// here detect inconsistent updates, not invalid PoW, difficulty, time, or work.
///
/// This slice starts at the anchor and supports forward updates and retained
/// suffix replacement only. The raw API does not backfill, retain validation
/// context, or promise that retained ancestry suffices for consensus checks.
/// Use `ValidatedHeaderStore` for contextual admission and separately stored
/// checkpoints. Raw and validated stores cannot open each other's mode.
///
/// Each update is one immediately durable redb transaction. There are no exposed
/// transactions or background tasks. Drop releases the database; embedding callers
/// should run this blocking API away from latency-sensitive executor threads.
/// Live header records and update batches are bounded by the configured capacity.
/// redb bookkeeping, free pages and transaction overhead are not a byte-size or
/// host RSS guarantee, and pruning does not necessarily shrink the file.
pub struct HeaderStore {
    /// Exclusive database handle; only this module writes its tables.
    database: Database,
    /// Caller-selected limits, validated against disk on open.
    config: StoreConfig,
    /// Immutable expected identity, never reconstructed from untrusted peers.
    anchor: StoreAnchor,
    /// Validated mode is inaccessible through public raw-store constructors.
    validated: bool,
}

impl HeaderStore {
    /// Read the pinned identity from local disk, never from a bootstrap oracle.
    pub(crate) fn open_pinned(path: &Path, config: StoreConfig) -> Result<Self, StoreError> {
        let database = Database::builder()
            .set_cache_size(config.cache_bytes.get())
            .open(path)?;
        let anchor = {
            let read = database.begin_read()?;
            read_metadata(&read.open_table(META)?)?.anchor()?
        };
        if anchor.network != bitcoin::Network::Bitcoin {
            return Err(StoreError::ConfigurationMismatch);
        }
        let store = Self {
            database,
            config,
            anchor,
            validated: true,
        };
        store.check_consistency()?;
        Ok(store)
    }

    /// Add already authenticated history while the database is still private.
    pub(crate) fn insert_history(
        &mut self,
        first: u64,
        history: &[Header],
    ) -> Result<(), StoreError> {
        if !self.validated
            || first > self.anchor.height
            || history.len() as u64 != self.anchor.height - first + 1
            || history.len() as u64 > self.capacity()
        {
            return Err(StoreError::InvalidUpdate("invalid bootstrap history range"));
        }
        let mut write = self.database.begin_write()?;
        write.set_durability(redb::Durability::Immediate)?;
        {
            let mut headers = write.open_table(HEADERS)?;
            let mut table = write.open_table(META)?;
            let mut metadata = read_metadata(&table)?;
            for (offset, header) in history.iter().enumerate() {
                headers.insert(first + offset as u64, encode_header(header).as_slice())?;
            }
            metadata.first = first;
            table.insert(0, metadata.encode().as_slice())?;
        }
        write.commit()?;
        self.check_consistency()
    }

    /// Create a new store containing only the supplied anchor header.
    ///
    /// Existing files are never overwritten. Failed creation may leave a file;
    /// inspect and remove it explicitly rather than silently reinitializing it.
    pub fn create(
        path: impl AsRef<Path>,
        config: StoreConfig,
        anchor: StoreAnchor,
        header: Header,
    ) -> Result<Self, StoreError> {
        Self::create_inner(path.as_ref(), config, anchor, header, None)
    }

    pub(crate) fn create_inner(
        path: &Path,
        config: StoreConfig,
        anchor: StoreAnchor,
        header: Header,
        context: Option<&HeaderContext>,
    ) -> Result<Self, StoreError> {
        if header.block_hash() != anchor.hash {
            return Err(StoreError::InvalidUpdate("anchor header hash mismatch"));
        }
        if anchor.height == 1 && anchor.hash != anchor.height_one_hash {
            return Err(StoreError::InvalidUpdate("height-one identity mismatch"));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        let database = Database::builder()
            .set_cache_size(config.cache_bytes.get())
            .create_file(file)?;
        let mut write = database.begin_write()?;
        write.set_durability(redb::Durability::Immediate)?;
        {
            let mut metadata = write.open_table(META)?;
            let mut headers = write.open_table(HEADERS)?;
            let mut contexts = write.open_table(CONTEXTS)?;
            let mut record = Metadata::new(anchor, config.header_capacity.get());
            record.validated = context.is_some();
            if let Some(context) = context {
                contexts.insert(anchor.height, context.encode().as_slice())?;
            }
            metadata.insert(0, record.encode().as_slice())?;
            headers.insert(anchor.height, encode_header(&header).as_slice())?;
        }
        write.commit()?;
        Ok(Self {
            database,
            config,
            anchor,
            validated: context.is_some(),
        })
    }

    /// Open existing storage and check the entire bounded suffix for consistency.
    ///
    /// Reject missing, malformed or incompatible application records without
    /// repairing, migrating or re-anchoring them. redb may perform its own crash
    /// recovery before these checks. The expected anchor and capacity must match;
    /// the cache budget may change. This is structural checking, not consensus
    /// validation or protection against malicious database replacement.
    pub fn open(
        path: impl AsRef<Path>,
        config: StoreConfig,
        anchor: StoreAnchor,
    ) -> Result<Self, StoreError> {
        Self::open_inner(path.as_ref(), config, anchor, false)
    }

    pub(crate) fn open_inner(
        path: &Path,
        config: StoreConfig,
        anchor: StoreAnchor,
        validated: bool,
    ) -> Result<Self, StoreError> {
        let database = Database::builder()
            .set_cache_size(config.cache_bytes.get())
            .open(path)?;
        let store = Self {
            database,
            config,
            anchor,
            validated,
        };
        store.check_consistency()?;
        Ok(store)
    }

    /// Return the pinned identity and anchor, independent of the rolling window.
    pub fn anchor(&self) -> StoreAnchor {
        self.anchor
    }

    /// Return the actual inclusive available bounds `(first, tip)`.
    ///
    /// Startup at a nonzero anchor and rollback after pruning can leave fewer
    /// headers than the ideal `RetainedRange`; old history is never recovered.
    pub fn bounds(&self) -> Result<(u64, u64), StoreError> {
        let read = self.database.begin_read()?;
        let metadata = read_metadata(&read.open_table(META)?)?;
        Ok((metadata.first, metadata.tip))
    }

    /// Read a canonical header, distinguishing expired history from future heights.
    pub fn header(&self, height: u64) -> Result<Header, StoreError> {
        let read = self.database.begin_read()?;
        let metadata = read_metadata(&read.open_table(META)?)?;
        if height < metadata.first {
            return Err(StoreError::OutsideRetainedHistory);
        }
        if metadata.tip < height {
            return Err(StoreError::BeyondTip);
        }
        read_header(&read.open_table(HEADERS)?, height)
    }

    /// Scan at most the configured retained capacity in one read transaction.
    pub(crate) fn height_for_hash(
        &self,
        hash: bitcoin::BlockHash,
    ) -> Result<Option<u64>, StoreError> {
        let read = self.database.begin_read()?;
        let metadata = read_metadata(&read.open_table(META)?)?;
        let headers = read.open_table(HEADERS)?;
        for height in (metadata.first..=metadata.tip).rev() {
            if read_header(&headers, height)?.block_hash() == hash {
                return Ok(Some(height));
            }
        }
        Ok(None)
    }

    /// Atomically replace everything after a retained parent and prune old headers.
    ///
    /// Passing the tip appends; an empty slice truncates to the parent. The parent
    /// itself is never replaced. A parent outside the retained suffix returns
    /// `RecoveryRequired`, including attempts to replace or cross the anchor.
    /// At most `header_capacity` headers may be submitted in one call.
    ///
    /// Callers choose branches and coordinate rollback with consumers; storage
    /// neither compares cumulative work nor decides whether rollback is safe.
    /// No automatic recovery or finalized-history promise is implied.
    pub fn replace_suffix(
        &mut self,
        parent: u64,
        replacement: &[Header],
    ) -> Result<(), StoreError> {
        if self.validated {
            return Err(StoreError::InvalidUpdate(
                "validated store requires contextual admission",
            ));
        }
        self.replace_inner(parent, replacement, None)
    }

    pub(crate) fn replace_inner(
        &mut self,
        parent: u64,
        replacement: &[Header],
        contexts: Option<&[HeaderContext]>,
    ) -> Result<(), StoreError> {
        if contexts.is_some() != self.validated
            || contexts.is_some_and(|contexts| contexts.len() != replacement.len())
        {
            return Err(StoreError::InvalidUpdate("validation mode mismatch"));
        }
        let count = u64::try_from(replacement.len())
            .map_err(|_| StoreError::InvalidUpdate("batch length overflow"))?;
        if count > self.config.header_capacity.get() {
            return Err(StoreError::InvalidUpdate("batch exceeds header capacity"));
        }
        let tip = parent
            .checked_add(count)
            .ok_or(StoreError::InvalidUpdate("height overflow"))?;
        let mut write = self.database.begin_write()?;
        write.set_durability(redb::Durability::Immediate)?;
        {
            let mut metadata_table = write.open_table(META)?;
            let mut metadata = read_metadata(&metadata_table)?;
            if parent < metadata.first || metadata.tip < parent {
                return Err(StoreError::RecoveryRequired);
            }
            if self.validated && parent < metadata.protected {
                return Err(StoreError::RecoveryRequired);
            }
            let mut headers = write.open_table(HEADERS)?;
            let mut context_table = write.open_table(CONTEXTS)?;
            let mut previous = read_header(&headers, parent)?.block_hash();
            // Write as we check: a late linkage error drops the transaction,
            // rolling back even already-inserted replacement headers.
            for (offset, header) in replacement.iter().enumerate() {
                if header.prev_blockhash != previous {
                    return Err(StoreError::InvalidUpdate("header linkage mismatch"));
                }
                let height = parent + offset as u64 + 1;
                if height == 1 && header.block_hash() != self.anchor.height_one_hash {
                    return Err(StoreError::InvalidUpdate("height-one identity mismatch"));
                }
                headers.insert(height, encode_header(header).as_slice())?;
                if let Some(contexts) = contexts {
                    context_table.insert(height, contexts[offset].encode().as_slice())?;
                }
                previous = header.block_hash();
            }
            let first = metadata
                .first
                .max(RetainedRange::new(tip, self.config.header_capacity).first());
            // Keep each retained header and its checkpoint together; neither
            // pruning nor replacement changes pinned identity or protection.
            headers.retain(|height, _| first <= height && height <= tip)?;
            context_table.retain(|height, _| first <= height && height <= tip)?;
            metadata.first = first;
            metadata.tip = tip;
            metadata.tip_hash = previous.to_byte_array();
            metadata_table.insert(0, metadata.encode().as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    pub(crate) fn context(&self, height: u64) -> Result<HeaderContext, StoreError> {
        let read = self.database.begin_read()?;
        let table = read.open_table(CONTEXTS)?;
        let bytes = table.get(height)?.ok_or(StoreError::RecoveryRequired)?;
        HeaderContext::decode(bytes.value(), self.anchor)
    }

    pub(crate) fn capacity(&self) -> u64 {
        self.config.header_capacity.get()
    }

    pub(crate) fn protected(&self) -> Result<u64, StoreError> {
        let read = self.database.begin_read()?;
        Ok(read_metadata(&read.open_table(META)?)?.protected)
    }

    pub(crate) fn protect(
        &mut self,
        height: u64,
        hash: bitcoin::BlockHash,
    ) -> Result<(), StoreError> {
        let mut write = self.database.begin_write()?;
        write.set_durability(redb::Durability::Immediate)?;
        {
            let mut table = write.open_table(META)?;
            let mut metadata = read_metadata(&table)?;
            if !self.validated || height < metadata.protected || height > metadata.tip {
                return Err(StoreError::InvalidUpdate("invalid protection height"));
            }
            if height < metadata.first {
                return Err(StoreError::OutsideRetainedHistory);
            }
            if read_header(&write.open_table(HEADERS)?, height)?.block_hash() != hash {
                return Err(StoreError::InvalidUpdate("protection hash mismatch"));
            }
            metadata.protected = height;
            table.insert(0, metadata.encode().as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    fn check_consistency(&self) -> Result<(), StoreError> {
        let read = self.database.begin_read()?;
        // A format bump must precede interpreting any future table layouts.
        let metadata_table = read.open_table(META)?;
        if metadata_table.len()? != 1 {
            return Err(StoreError::Corrupt("unexpected metadata records"));
        }
        let metadata = read_metadata(&metadata_table)?;
        // Raw and validated modes share v2 but cannot open each other's stores.
        if read.list_tables()?.count() != 3 || read.list_multimap_tables()?.next().is_some() {
            return Err(StoreError::Corrupt("unexpected tables"));
        }
        if !metadata.matches(self.anchor, self.config.header_capacity.get()) {
            return Err(StoreError::ConfigurationMismatch);
        }
        if metadata.validated != self.validated {
            return Err(StoreError::ConfigurationMismatch);
        }
        if metadata.protected < self.anchor.height || metadata.protected > metadata.tip {
            return Err(StoreError::Corrupt("invalid protected height"));
        }
        if (!self.validated && metadata.first < metadata.anchor_height)
            || metadata.tip < metadata.first
            || metadata.tip < metadata.anchor_height
        {
            return Err(StoreError::Corrupt("invalid height bounds"));
        }
        let count = metadata
            .tip
            .checked_sub(metadata.first)
            .and_then(|span| span.checked_add(1))
            .ok_or(StoreError::Corrupt("height span overflow"))?;
        let headers = read.open_table(HEADERS)?;
        let contexts = read.open_table(CONTEXTS)?;
        let context_count = if self.validated {
            metadata.tip - metadata.first.max(metadata.anchor_height) + 1
        } else {
            0
        };
        if contexts.len()? != context_count {
            return Err(StoreError::Corrupt("invalid context count"));
        }
        if count > metadata.capacity || headers.len()? != count {
            return Err(StoreError::Corrupt("invalid retained header count"));
        }
        let mut previous = None;
        let mut last_hash = None;
        let mut previous_context: Option<HeaderContext> = None;
        for (offset, entry) in headers.iter()?.enumerate() {
            let (height, bytes) = entry?;
            let height = height.value();
            if height != metadata.first + offset as u64 {
                return Err(StoreError::Corrupt("noncontiguous heights"));
            }
            let header = decode_header(bytes.value())?;
            let hash = header.block_hash();
            if self.validated && height >= self.anchor.height {
                let bytes = contexts
                    .get(height)?
                    .ok_or(StoreError::Corrupt("missing context"))?;
                let context = HeaderContext::decode(bytes.value(), self.anchor)?;
                if context.height() != height || context.tip() != header {
                    return Err(StoreError::Corrupt("context header mismatch"));
                }
                if let Some(previous) = previous_context {
                    // Historical future-time checks used the original clock.
                    // Recompute all durable rules without imposing today's clock.
                    let expected = previous
                        .validate_next(header, u64::MAX)
                        .map_err(|_| StoreError::Corrupt("invalid contextual successor"))?;
                    if expected.encode() != context.encode() {
                        return Err(StoreError::Corrupt("context transition mismatch"));
                    }
                }
                previous_context = Some(context);
            }
            if previous.is_some_and(|previous| header.prev_blockhash != previous) {
                return Err(StoreError::Corrupt("broken header linkage"));
            }
            if height == self.anchor.height && hash != self.anchor.hash {
                return Err(StoreError::Corrupt("anchor header mismatch"));
            }
            if height == 1 && hash != self.anchor.height_one_hash {
                return Err(StoreError::Corrupt("height-one identity mismatch"));
            }
            previous = Some(hash);
            last_hash = Some(hash.to_byte_array());
        }
        if last_hash != Some(metadata.tip_hash) {
            return Err(StoreError::Corrupt("tip hash mismatch"));
        }
        Ok(())
    }
}

fn read_metadata(table: &impl ReadableTable<u8, &'static [u8]>) -> Result<Metadata, StoreError> {
    let bytes = table
        .get(0)?
        .ok_or(StoreError::Corrupt("missing metadata"))?;
    Metadata::decode(bytes.value())
}

fn read_header(
    table: &impl ReadableTable<u64, &'static [u8]>,
    height: u64,
) -> Result<Header, StoreError> {
    let bytes = table
        .get(height)?
        .ok_or(StoreError::Corrupt("missing header"))?;
    decode_header(bytes.value())
}
