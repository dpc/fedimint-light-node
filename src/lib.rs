//! Building blocks for an embeddable, bounded-history guardian Bitcoin backend.
//!
//! This bootstrap only implements retention-window arithmetic. It does not
//! connect to Bitcoin, persist headers, or validate blocks.

pub mod retained_range;
