use std::num::NonZeroU64;

/// Caller-selected fee information, never a network estimate or confirmation promise.
///
/// This policy is not persisted: supply it on each service construction. It does
/// not inspect transaction fees or alter submission admission. There is no mempool,
/// input-value lookup, HTTP fee service, or adaptive default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FeePolicy {
    /// No fee information is available.
    #[default]
    Unavailable,
    /// A positive operator-supplied static rate, not an observed network rate.
    Fixed {
        /// Satoshis per 1,000 virtual bytes, NOT sat/vB or sat/kwu.
        sats_per_kvb: NonZeroU64,
    },
}

impl FeePolicy {
    /// Return the configured rate unchanged, or no information by default.
    ///
    /// No unit conversion or multiplication is performed, including at `u64::MAX`.
    pub fn sats_per_kvb(self) -> Option<NonZeroU64> {
        match self {
            Self::Unavailable => None,
            Self::Fixed { sats_per_kvb } => Some(sats_per_kvb),
        }
    }
}
