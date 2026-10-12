use std::fs::OpenOptions;
use std::path::Path;

use bitcoin::block::Header;
use bitcoin::hashes::Hash;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata};

use crate::retained_range::RetainedRange;
use crate::store_anchor::StoreAnchor;
use crate::store_config::StoreConfig;
use crate::store_error::StoreError;
use crate::store_format::{HEADERS, META, Metadata, decode_header, encode_header};

/// Synchronous, bounded canonical-header persistence for an embedding caller.
///
/// **Not a validator:** the caller must authenticate the anchor, validate every
/// header, and select a branch before storing it (SPEC-chain-trust). Link checks
/// here detect inconsistent updates, not invalid PoW, difficulty, time, or work.
///
/// This slice starts at the anchor and supports forward updates and retained
/// suffix replacement only. It does not backfill, retain validation context, or
/// promise that retained ancestry suffices for consensus checks. Future bounded
/// context must live separately; pruning here touches only canonical headers.
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
}

impl HeaderStore {
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
            metadata.insert(
                0,
                Metadata::new(anchor, config.header_capacity.get())
                    .encode()
                    .as_slice(),
            )?;
            headers.insert(anchor.height, encode_header(&header).as_slice())?;
        }
        write.commit()?;
        Ok(Self {
            database,
            config,
            anchor,
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
        let database = Database::builder()
            .set_cache_size(config.cache_bytes.get())
            .open(path)?;
        let store = Self {
            database,
            config,
            anchor,
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
            let mut headers = write.open_table(HEADERS)?;
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
                previous = header.block_hash();
            }
            let first = metadata
                .first
                .max(RetainedRange::new(tip, self.config.header_capacity).first());
            // Delete only rows outside the new canonical interval, not other
            // tables that might eventually hold separately bounded context.
            headers.retain(|height, _| first <= height && height <= tip)?;
            metadata.first = first;
            metadata.tip = tip;
            metadata.tip_hash = previous.to_byte_array();
            metadata_table.insert(0, metadata.encode().as_slice())?;
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
        // Schema v1 owns exactly these two tables. Future validation context
        // needs a versioned extension, not silent interpretation by old code.
        if read.list_tables()?.count() != 2 || read.list_multimap_tables()?.next().is_some() {
            return Err(StoreError::Corrupt("unexpected tables"));
        }
        if !metadata.matches(self.anchor, self.config.header_capacity.get()) {
            return Err(StoreError::ConfigurationMismatch);
        }
        if metadata.first < metadata.anchor_height || metadata.tip < metadata.first {
            return Err(StoreError::Corrupt("invalid height bounds"));
        }
        let count = metadata
            .tip
            .checked_sub(metadata.first)
            .and_then(|span| span.checked_add(1))
            .ok_or(StoreError::Corrupt("height span overflow"))?;
        let headers = read.open_table(HEADERS)?;
        if count > metadata.capacity || headers.len()? != count {
            return Err(StoreError::Corrupt("invalid retained header count"));
        }
        let mut previous = None;
        let mut last_hash = None;
        for (offset, entry) in headers.iter()?.enumerate() {
            let (height, bytes) = entry?;
            let height = height.value();
            if height != metadata.first + offset as u64 {
                return Err(StoreError::Corrupt("noncontiguous heights"));
            }
            let header = decode_header(bytes.value())?;
            let hash = header.block_hash();
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
