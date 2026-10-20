//! Building blocks for an embeddable, bounded-history guardian Bitcoin backend.
//!
//! This library implements retention-window arithmetic and a bounded persistent
//! header store, bounded v1 block frames and decoding, stateless integrity checks,
//! declared-target PoW checks, and separate contextual mainnet header admission
//! with relative-work selection and persistent bounded context. Initial HTTPS
//! source agreement establishes an explicitly trusted mainnet checkpoint and P2P
//! authenticates bounded recent ancestry into it. No API establishes full consensus
//! validity or objective freshness. Outbound mainnet header synchronization and
//! on-demand witness-bearing block retrieval continue without those sources.
//! Explicitly configured periodic Esplora candidates can independently advance
//! supported higher-work headers, never replace the pinned trust origin.
//! The same exclusive owner submits bounded caller-owned transactions; a completed
//! local write establishes neither peer acceptance, propagation nor confirmation.
//! An optional bounded loopback RPC profile supports guardian clients, with
//! explicitly opt-in synthetic status and no automatic consumer finality binding.

pub mod block_decode;
pub mod block_download_config;
pub mod block_download_error;
pub mod block_integrity;
pub mod bootstrap;
pub mod bootstrap_config;
pub mod bootstrap_error;
pub mod declared_pow;
pub mod decode_error;
pub mod explorer_header_config;
mod explorer_headers;
mod explorer_http;
#[cfg(test)]
mod explorer_test_support;
mod fee_observation;
pub mod fee_observation_status;
pub mod fee_policy;
pub mod frame_error;
pub mod guardian_binding;
pub mod guardian_rpc;
pub mod guardian_rpc_config;
pub mod header_context;
pub mod header_error;
pub mod header_store;
pub mod header_sync;
mod idle_peer;
pub mod integrity_error;
mod inventory_observer;
pub mod mempool_observation;
mod output_amount_cache;
mod peer_addr_wire;
mod peer_address;
mod peer_book;
mod peer_cache;
mod peer_connection;
mod peer_pool;
pub mod peer_pool_config;
#[cfg(all(test, target_os = "linux"))]
mod readiness_observer;
#[cfg(all(test, target_os = "linux"))]
mod readiness_transport_tests;
pub mod retained_range;
mod rpc_http;
#[cfg(test)]
mod rpc_http_tests;
mod rpc_method;
#[cfg(test)]
mod rpc_method_tests;
pub mod store_anchor;
pub mod store_config;
pub mod store_error;
pub mod sync_config;
pub mod sync_error;
pub mod transaction_decode;
mod transaction_download;
pub mod transaction_submission_config;
pub mod transaction_submission_report;
pub mod v1_block_frame;
pub mod validated_header_store;

mod block_download;
mod bootstrap_history;
mod bootstrap_http;
mod encoded_size_limit;
mod header_wire;
mod peer_handshake;
mod store_format;
mod transaction_submission;
