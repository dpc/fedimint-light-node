use std::num::NonZeroU64;

/// Caller-selected source of fee information.
///
/// This policy is not persisted: supply it on each service construction. It does
/// not alter submission admission or trigger network requests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FeePolicy {
    /// No fee information is available.
    #[default]
    Unavailable,
    /// Use the qualified fixed historical sample's finite median upper endpoint.
    Observed,
    /// A positive operator-supplied static rate, not an observed network rate.
    Fixed {
        /// Satoshis per 1,000 virtual bytes, NOT sat/vB or sat/kwu.
        sats_per_kvb: NonZeroU64,
    },
}

impl FeePolicy {
    /// Return only the configured fixed rate; HeaderSync resolves observed policy.
    ///
    /// No unit conversion or multiplication is performed, including at `u64::MAX`.
    pub fn sats_per_kvb(self) -> Option<NonZeroU64> {
        match self {
            Self::Unavailable | Self::Observed => None,
            Self::Fixed { sats_per_kvb } => Some(sats_per_kvb),
        }
    }
}
