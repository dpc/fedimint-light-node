use crate::frame_error::FrameError;
use crate::integrity_error::IntegrityError;
use crate::store_error::StoreError;
use crate::sync_error::SyncError;

/// Retention/selection/local failures are distinct from transient peer availability.
#[derive(Debug, thiserror::Error)]
pub enum BlockDownloadError {
    /// Caller supplied an invalid resource budget.
    #[error("invalid block download configuration")]
    Configuration,
    /// The requested height has been pruned or predates queryable history.
    #[error("block is outside retained history")]
    OutsideRetainedHistory,
    /// The requested height is above the selected tip.
    #[error("block height is beyond selected tip")]
    BeyondTip,
    /// The caller's hash is not selected at that retained height.
    #[error("requested block is not canonical at this height")]
    NotCanonical,
    /// A previous terminal state failure stopped the service.
    #[error("service stopped; explicit recovery required")]
    Stopped,
    /// Local storage failed; no further peers are tried.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Even a trusted anchor must satisfy declared mainnet PoW before body retrieval.
    #[error(transparent)]
    HeaderIntegrity(#[from] IntegrityError),
    /// Bounded attempts failed; this says nothing about chain freshness or validity.
    #[error("block unavailable after {attempts} attempts: {last}")]
    Unavailable {
        /// Number of connection attempts started, including a cancelled deadline.
        attempts: usize,
        /// Final observed failure (not a durable peer reputation judgment).
        last: BlockPeerError,
    },
}

/// Why a single peer or a whole-operation resource budget failed.
#[derive(Debug, thiserror::Error)]
pub enum BlockPeerError {
    /// Connect, handshake or response failed bounded protocol admission.
    #[error(transparent)]
    Peer(#[from] SyncError),
    /// Body checksum, encoding or mechanical integrity failed.
    #[error(transparent)]
    Body(#[from] FrameError),
    /// Peer does not advertise both complete history and witness serving.
    #[error("peer lacks NODE_NETWORK or NODE_WITNESS")]
    Services,
    /// Any notfound ends the visit, even an irrelevant or malformed inventory.
    #[error("peer declined requested data")]
    NotFound,
    /// The absolute per-visit or whole-operation deadline expired.
    #[error("block download deadline expired")]
    Timeout,
    /// No further frame header fits in the aggregate received-byte budget.
    #[error("aggregate block download byte budget exhausted")]
    ByteBudget,
}
