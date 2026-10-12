//! Building blocks for an embeddable, bounded-history guardian Bitcoin backend.
//!
//! This library implements retention-window arithmetic and a bounded persistent
//! header store. Storage checks structural linkage, not header consensus validity
//! or bootstrap authenticity. There is no networking or working node.

pub mod header_store;
pub mod retained_range;
pub mod store_anchor;
pub mod store_config;
pub mod store_error;

mod store_format;
