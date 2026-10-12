//! Building blocks for an embeddable, bounded-history guardian Bitcoin backend.
//!
//! This library implements retention-window arithmetic and a bounded persistent
//! header store, bounded v1 block frames and decoding, stateless integrity checks,
//! declared-target PoW checks, and separate contextual mainnet header admission
//! with relative-work selection and persistent bounded context. No API establishes
//! full consensus validity or bootstrap authenticity. Bounded outbound mainnet
//! header synchronization requires caller-trusted initial state; body retrieval,
//! bootstrap authentication and guardian integration are not implemented.

pub mod block_decode;
pub mod block_integrity;
pub mod declared_pow;
pub mod decode_error;
pub mod frame_error;
pub mod header_context;
pub mod header_error;
pub mod header_store;
pub mod header_sync;
pub mod integrity_error;
pub mod retained_range;
pub mod store_anchor;
pub mod store_config;
pub mod store_error;
pub mod sync_config;
pub mod sync_error;
pub mod v1_block_frame;
pub mod validated_header_store;

mod encoded_size_limit;
mod header_wire;
mod store_format;
