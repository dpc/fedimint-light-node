use bitcoin::{BlockHash, Network};

/// Caller-supplied chain identity and immutable bootstrap anchor.
///
/// Storage does not establish source agreement or authenticate this information.
/// The height-one identity is pinned independently of the rolling header window
/// for a future guardian adapter (REQ-guardian-backend).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreAnchor {
    /// Network selected by the embedding caller, not a supported-network policy.
    pub network: Network,
    /// Caller-authenticated height-one hash used as chain identity.
    pub height_one_hash: BlockHash,
    /// Height of the initial header; this slice does not backfill older headers.
    pub height: u64,
    /// Hash of the initial header.
    pub hash: BlockHash,
}
