//! Bounded admission of complete block payloads, not P2P message envelopes.
//!
//! An allocation-free structural pass precedes rust-bitcoin decoding. This avoids
//! its speculative vector reservations on truncated counts and witness indexes.
//! CompactSize parsing and object construction remain owned by rust-bitcoin.

use bitcoin::consensus::encode::VarInt;
use bitcoin::consensus::{Decodable, deserialize};
use bitcoin::{Block, BlockHash};

use crate::block_integrity::{WitnessRules, check_block_integrity};
use crate::decode_error::DecodeError;

/// Per-call wire and structural budgets; neither is a global memory/RSS limit.
#[derive(Debug, Clone, Copy)]
pub struct BlockDecodeLimits {
    /// Maximum payload bytes, additionally capped at the protocol's 4 MB.
    pub max_payload_bytes: usize,
    /// Maximum combined transactions, inputs, outputs, and witness stack items.
    ///
    /// Empty witness items count, as do entries in every nested vector. Scripts
    /// and witness contents are bounded by payload bytes instead. No defaults
    /// are chosen: callers must budget concurrency and retained results separately.
    pub max_elements: usize,
}

/// Decode one complete canonical payload and check its mechanical integrity.
///
/// Before constructing a block, reject excess bytes/elements, impossible lengths,
/// truncation, nonminimal CompactSize values, unsupported witness flags, superfluous
/// witness serialization and trailing bytes. The structural pass allocates nothing
/// and takes O(payload bytes) work, with fixed nesting depth.
///
/// Successful preflight bounds decoded storage by O(payload bytes + max_elements),
/// not an exact allocation-byte budget. In rust-bitcoin 0.32.102, transaction/input/
/// output vectors reserve no more than their verified counts initially; scripts
/// grow in chunks no larger than their verified lengths; each nonempty witness
/// reserves four bytes per item plus 128 bytes, then grows for verified contents.
/// Vector spare capacity, growth transients and allocator overhead are not counted
/// as structural elements. Dependency upgrades must preserve these properties.
///
/// The borrowed payload, returned block, integrity scratch space, concurrent calls
/// and transport buffers need caller accounting. Allocation failure is not recovered.
/// This does not authenticate `requested_hash`, choose activation rules, check PoW
/// or expected difficulty, or establish full consensus validity.
pub fn decode_block(
    payload: &[u8],
    requested_hash: BlockHash,
    witness_rules: WitnessRules,
    limits: BlockDecodeLimits,
) -> Result<Block, DecodeError> {
    preflight(payload, limits)?;
    let block = deserialize(payload)?;
    #[cfg(all(test, target_os = "linux"))]
    crate::readiness_observer::observe("decoded");
    check_block_integrity(
        &block,
        requested_hash,
        witness_rules,
        limits.max_payload_bytes,
    )?;
    #[cfg(all(test, target_os = "linux"))]
    crate::readiness_observer::observe("checked");
    Ok(block)
}

/// Scan lengths and structure without creating any decoded collections.
fn preflight(mut bytes: &[u8], limits: BlockDecodeLimits) -> Result<(), DecodeError> {
    if bytes.len() > limits.max_payload_bytes.min(4_000_000) {
        return Err(DecodeError::PayloadLimit);
    }
    let mut remaining = limits.max_elements;
    skip(&mut bytes, 80)?;
    let transactions = count(&mut bytes, &mut remaining, 10)?;
    for _ in 0..transactions {
        preflight_transaction(&mut bytes, &mut remaining)?;
    }
    if !bytes.is_empty() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(())
}

/// Scan one transaction without allocation, shared with raw submission admission.
pub(crate) fn preflight_transaction(
    bytes: &mut &[u8],
    remaining: &mut usize,
) -> Result<(), DecodeError> {
    skip(bytes, 4)?;
    let first_inputs = compact_size(bytes)?;
    let witness = first_inputs == 0;
    let inputs = if witness {
        let flag = u8::consensus_decode(bytes)?;
        if flag != 1 {
            return Err(DecodeError::Encoding(
                bitcoin::consensus::encode::Error::UnsupportedSegwitFlag(flag),
            ));
        }
        compact_size(bytes)?
    } else {
        first_inputs
    };
    charge(inputs, remaining, bytes.len(), 41)?;
    for _ in 0..inputs {
        skip(bytes, 36)?;
        blob(bytes)?;
        skip(bytes, 4)?;
    }
    let outputs = count(bytes, remaining, 9)?;
    for _ in 0..outputs {
        skip(bytes, 8)?;
        blob(bytes)?;
    }
    if witness {
        let mut has_witness = false;
        for _ in 0..inputs {
            let items = count(bytes, remaining, 1)?;
            has_witness |= items != 0;
            for _ in 0..items {
                blob(bytes)?;
            }
        }
        if !has_witness {
            return Err(DecodeError::SuperfluousWitness);
        }
    }
    skip(bytes, 4)?;
    Ok(())
}

/// Reuse the dependency's canonical CompactSize decoder, without narrowing casts.
fn compact_size(bytes: &mut &[u8]) -> Result<usize, DecodeError> {
    usize::try_from(VarInt::consensus_decode(bytes)?.0).map_err(|_| DecodeError::LengthOverflow)
}

/// Charge a collection before iteration or any downstream allocation.
fn count(
    bytes: &mut &[u8],
    remaining: &mut usize,
    minimum_bytes: usize,
) -> Result<usize, DecodeError> {
    let count = compact_size(bytes)?;
    charge(count, remaining, bytes.len(), minimum_bytes)?;
    Ok(count)
}

/// Bound aggregate elements and reject counts that cannot fit the remaining wire.
fn charge(
    count: usize,
    remaining: &mut usize,
    bytes: usize,
    minimum_bytes: usize,
) -> Result<(), DecodeError> {
    *remaining = remaining
        .checked_sub(count)
        .ok_or(DecodeError::ElementLimit)?;
    if count > bytes / minimum_bytes {
        return Err(DecodeError::Truncated);
    }
    Ok(())
}

/// Skip a length-prefixed script or witness element without allocating.
fn blob(bytes: &mut &[u8]) -> Result<(), DecodeError> {
    let len = compact_size(bytes)?;
    skip(bytes, len)
}

/// Advance only after checking that the entire field is present.
fn skip(bytes: &mut &[u8], len: usize) -> Result<(), DecodeError> {
    *bytes = bytes.get(len..).ok_or(DecodeError::Truncated)?;
    Ok(())
}
