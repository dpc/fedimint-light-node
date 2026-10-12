//! Private schema v2: fixed-size bincode records, little-endian fixed integers.
//!
//! Bitcoin headers use their consensus 80-byte representation inside the record,
//! not a Rust struct's serde layout. Changes to these layouts require a schema
//! version bump; there is deliberately no migration or automatic reinitialization.

use bincode::{Decode, Encode};
use bitcoin::block::Header;
use bitcoin::hashes::Hash;
use redb::TableDefinition;

use crate::store_anchor::StoreAnchor;
use crate::store_error::StoreError;

pub(crate) const META: TableDefinition<u8, &[u8]> = TableDefinition::new("metadata");
pub(crate) const HEADERS: TableDefinition<u64, &[u8]> = TableDefinition::new("headers");
pub(crate) const CONTEXTS: TableDefinition<u64, &[u8]> = TableDefinition::new("contexts");
const VERSION: u32 = 2;
const MAGIC: [u8; 8] = *b"FLNSTORE";
const MAX_RECORD_BYTES: usize = 256;

/// Fixed-size metadata, independent of prunable headers and future context tables.
#[derive(Clone, Decode, Encode)]
pub(crate) struct Metadata {
    /// Application identifier, not redb's own database format identifier.
    magic: [u8; 8],
    /// Version of every table and record in this application database.
    version: u32,
    /// Bitcoin network's wire magic.
    network: [u8; 4],
    /// Pinned height-one identity, even when it is not in the header table.
    height_one_hash: [u8; 32],
    /// Immutable bootstrap height.
    pub(crate) anchor_height: u64,
    /// Immutable bootstrap hash.
    anchor_hash: [u8; 32],
    /// Creation-time retention capacity.
    pub(crate) capacity: u64,
    /// Actual first available header; rollback never restores pruned history.
    pub(crate) first: u64,
    /// Current selected tip height.
    pub(crate) tip: u64,
    /// Current selected tip hash, checked against the header table on open.
    pub(crate) tip_hash: [u8; 32],
    /// Whether writes require contextual validation, immutable after creation.
    pub(crate) validated: bool,
    /// Monotonic caller-protected height; never roll back through this height.
    pub(crate) protected: u64,
}

impl Metadata {
    /// Build the initial metadata for a store containing just its anchor.
    pub(crate) fn new(anchor: StoreAnchor, capacity: u64) -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            network: anchor.network.magic().to_bytes(),
            height_one_hash: anchor.height_one_hash.to_byte_array(),
            anchor_height: anchor.height,
            anchor_hash: anchor.hash.to_byte_array(),
            capacity,
            first: anchor.height,
            tip: anchor.height,
            tip_hash: anchor.hash.to_byte_array(),
            validated: false,
            protected: anchor.height,
        }
    }

    /// Check the immutable caller expectations before interpreting stored history.
    pub(crate) fn matches(&self, anchor: StoreAnchor, capacity: u64) -> bool {
        self.network == anchor.network.magic().to_bytes()
            && self.height_one_hash == anchor.height_one_hash.to_byte_array()
            && self.anchor_height == anchor.height
            && self.anchor_hash == anchor.hash.to_byte_array()
            && self.capacity == capacity
    }

    /// Encode the fixed-size application metadata under schema v2.
    pub(crate) fn encode(&self) -> Vec<u8> {
        // Fixed arrays and integers encoded into a Vec cannot fail.
        bincode::encode_to_vec(self, codec()).expect("fixed-size metadata encoding")
    }

    /// Reject foreign or incompatible records before decoding fixed-size fields.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() < 12 || bytes[..8] != MAGIC {
            return Err(StoreError::Corrupt("missing application format marker"));
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().expect("four bytes"));
        check_version(version)?;
        decode_exact(bytes)
    }
}

fn codec() -> impl bincode::config::Config {
    bincode::config::standard()
        .with_little_endian()
        .with_fixed_int_encoding()
        .with_limit::<MAX_RECORD_BYTES>()
}

fn decode_exact<T: Decode<()>>(bytes: &[u8]) -> Result<T, StoreError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(StoreError::Corrupt("record exceeds decode limit"));
    }
    let (record, consumed) = bincode::decode_from_slice(bytes, codec())
        .map_err(|_| StoreError::Corrupt("invalid bincode record"))?;
    if consumed != bytes.len() {
        return Err(StoreError::Corrupt("trailing record bytes"));
    }
    Ok(record)
}

fn check_version(version: u32) -> Result<(), StoreError> {
    if version != VERSION {
        return Err(StoreError::UnsupportedSchema(version));
    }
    Ok(())
}

/// Wrap the consensus header bytes in a versioned bincode record.
pub(crate) fn encode_header(header: &Header) -> Vec<u8> {
    let bytes: [u8; 80] = bitcoin::consensus::serialize(header)
        .try_into()
        .expect("Bitcoin headers are 80 bytes");
    bincode::encode_to_vec((VERSION, bytes), codec()).expect("fixed-size header encoding")
}

/// Decode exactly one versioned header with bounded work and no trailing bytes.
pub(crate) fn decode_header(bytes: &[u8]) -> Result<Header, StoreError> {
    let (version, header): (u32, [u8; 80]) = decode_exact(bytes)?;
    check_version(version)?;
    bitcoin::consensus::deserialize(&header)
        .map_err(|_| StoreError::Corrupt("invalid Bitcoin header encoding"))
}
