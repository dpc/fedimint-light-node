//! Building blocks for an embeddable, bounded-history guardian Bitcoin backend.
//!
//! This library implements retention-window arithmetic and a bounded persistent
//! header store, stateless block integrity checks, and separate declared-target
//! PoW checks. None establishes header consensus validity or bootstrap
//! authenticity. There is no networking or working node.

pub mod block_integrity;
pub mod declared_pow;
pub mod header_store;
pub mod integrity_error;
pub mod retained_range;
pub mod store_anchor;
pub mod store_config;
pub mod store_error;

mod encoded_size_limit;
mod store_format;
