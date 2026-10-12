//! Declared-target PoW only; expected difficulty requires separate chain context.

use bitcoin::{Target, block};

use crate::integrity_error::IntegrityError;

/// Check target range/encoding and PoW against a header's own declared target.
///
/// `pow_limit` must come from the caller's trusted network configuration. This
/// does not check expected difficulty, canonical nBits for a chain position,
/// timestamps, ancestry, signet solutions, or an authenticated anchor. It is
/// separate from body integrity and must not be called full header validation.
/// Noncanonical but positive, nonoverflowing encodings are not rejected merely
/// for failing a compact round trip, matching Core v30.0 `DeriveTarget`.
pub fn check_declared_pow(header: &block::Header, pow_limit: Target) -> Result<(), IntegrityError> {
    let bits = header.bits.to_consensus();
    let size = bits >> 24;
    let word = bits & 0x007f_ffff;
    // Core arith_uint256::SetCompact overflow rules. rust-bitcoin's target
    // conversion alone can truncate overflowing encodings.
    let overflow =
        word != 0 && (size > 34 || (word > 0xff && size > 33) || (word > 0xffff && size > 32));
    if bits & 0x0080_0000 != 0 || overflow {
        return Err(IntegrityError::DeclaredTarget);
    }
    let target = header.target();
    if target == Target::ZERO || target > pow_limit {
        return Err(IntegrityError::DeclaredTarget);
    }
    if !target.is_met_by(header.block_hash()) {
        return Err(IntegrityError::DeclaredProofOfWork);
    }
    Ok(())
}
