//! Inclusive query boundaries, independent of header-validation context.

use std::num::NonZeroU64;

/// The queryable suffix of a chain, measured in block headers rather than time.
///
/// Validation context may need older headers; it is not part of this range.
/// See REQ-bounded-history for the distinction between retention and availability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedRange {
    /// First queryable height, inclusive.
    first: u64,
    /// Last queryable height, inclusive.
    tip: u64,
}

impl RetainedRange {
    /// Derive a suffix containing at most `capacity` headers, including the tip.
    pub fn new(tip: u64, capacity: NonZeroU64) -> Self {
        Self {
            first: tip.saturating_sub(capacity.get() - 1),
            tip,
        }
    }

    /// Return the first queryable height.
    pub fn first(self) -> u64 {
        self.first
    }

    /// Return the last queryable height.
    pub fn tip(self) -> u64 {
        self.tip
    }

    /// Test query eligibility, without promising that a peer has the block body.
    pub fn contains(self, height: u64) -> bool {
        (self.first..=self.tip).contains(&height)
    }
}
