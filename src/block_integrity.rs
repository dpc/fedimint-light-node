//! Stateless checks on caller-owned, already decoded blocks.
//!
//! This is not a consensus validator or a wire decoder. Use [`crate::block_decode`]
//! for bounded canonical wire admission. The caller must authenticate the
//! requested header, and determine witness activation from trusted chain context.
//! No scripts, UTXO/spend checks, subsidy, contextual finality, BIP34 height,
//! signature-operation limits, signet solution, or chain selection are checked.
//! PoW is a separate operation in [`crate::declared_pow`].
//!
//! Semantics follow Bitcoin Core v30.0 `CheckTransaction`, `CheckMerkleRoot`,
//! `CheckWitnessMalleation` and BIP141. In particular, a present commitment must
//! be verified even if every witness is empty; rust-bitcoin 0.32.102's
//! `Block::check_witness_commitment` returns early in that case.

use std::collections::BTreeSet;

use bitcoin::consensus::Encodable;
use bitcoin::hashes::{Hash, HashEngine, sha256d};
use bitcoin::{Amount, Block, BlockHash, Transaction};

use crate::encoded_size_limit::EncodedSizeLimit;
use crate::integrity_error::IntegrityError;

/// Witness rules selected by the caller, not inferred from an untrusted block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WitnessRules {
    /// Before SegWit activation: forbid witnesses and ignore commitment patterns.
    BeforeActivation,
    /// After activation: enforce BIP141 commitments and absence rules.
    Active,
}

/// Check a block's mechanical integrity against its requested hash.
///
/// Enforces a nonempty block, unique first coinbase, nonempty transaction input
/// and output lists, coinbase script length, non-null ordinary inputs, distinct
/// inputs per transaction, and individual/total output money ranges per
/// transaction. Checks distinct txids, the transaction Merkle root (including
/// mutation), witness commitments under `witness_rules`, at most 1 MB stripped
/// size, 4 MB serialization and 4 million weight units.
///
/// `max_serialized_bytes` is an additional caller budget, never a relaxation of
/// protocol limits. Encoding into a counting sink rejects excess bytes before
/// hashing or allocating scratch trees/sets; it does not copy the block payload.
/// Temporary memory is O(transaction count + largest input count), bounded by
/// the accepted size. The caller owns the block's existing allocations (including
/// spare capacity); this API neither bounds decoding nor promises a process RSS
/// limit. Traversal of the supplied object remains synchronous.
///
/// Success does not check PoW, authenticate the requested hash, or establish
/// consensus validity. Every error rejects only this supplied body: never cache
/// it as proof that the requested header or another body for it is invalid.
pub fn check_block_integrity(
    block: &Block,
    requested: BlockHash,
    witness_rules: WitnessRules,
    max_serialized_bytes: usize,
) -> Result<(), IntegrityError> {
    if block.block_hash() != requested {
        return Err(IntegrityError::RequestedHash);
    }
    let mut size = EncodedSizeLimit::new(max_serialized_bytes.min(4_000_000));
    block
        .consensus_encode(&mut bitcoin::io::FromStd::new(&mut size))
        .map_err(|_| IntegrityError::SerializedSize)?;
    // The successful bounded encoding makes these usize sums safe. Weight is
    // at least four times stripped size, so this also enforces its 1 MB bound.
    if block.weight().to_wu() > 4_000_000 {
        return Err(IntegrityError::Weight);
    }
    let Some(coinbase) = block.txdata.first() else {
        return Err(IntegrityError::Coinbase);
    };
    if !coinbase.is_coinbase() {
        return Err(IntegrityError::Coinbase);
    }
    for (index, tx) in block.txdata.iter().enumerate() {
        if index != 0 && tx.is_coinbase() {
            return Err(IntegrityError::Coinbase);
        }
        check_transaction(tx)?;
    }
    let mut leaves: Vec<_> = block
        .txdata
        .iter()
        .map(|tx| tx.compute_txid().to_raw_hash())
        .collect();
    let mut distinct = BTreeSet::new();
    for hash in &leaves {
        if !distinct.insert(*hash) {
            return Err(IntegrityError::DuplicateTransaction);
        }
    }
    drop(distinct);
    if merkle_root(&mut leaves)? != block.header.merkle_root.to_raw_hash() {
        return Err(IntegrityError::MerkleRoot);
    }
    check_witness(block, witness_rules)
}

#[cfg(test)]
#[path = "block_integrity_tests.rs"]
mod tests;

fn check_transaction(tx: &Transaction) -> Result<(), IntegrityError> {
    if tx.input.is_empty() || tx.output.is_empty() {
        return Err(IntegrityError::EmptyTransaction);
    }
    if tx.is_coinbase() && !(2..=100).contains(&tx.input[0].script_sig.len()) {
        return Err(IntegrityError::CoinbaseScript);
    }
    let mut inputs = BTreeSet::new();
    for input in &tx.input {
        if !tx.is_coinbase() && input.previous_output.is_null() {
            return Err(IntegrityError::NullInput);
        }
        if !inputs.insert(input.previous_output) {
            return Err(IntegrityError::DuplicateInput);
        }
    }
    let mut total = 0_u64;
    for output in &tx.output {
        total = total
            .checked_add(output.value.to_sat())
            .filter(|total| *total <= Amount::MAX_MONEY.to_sat())
            .ok_or(IntegrityError::OutputValue)?;
    }
    Ok(())
}

fn merkle_root(hashes: &mut Vec<sha256d::Hash>) -> Result<sha256d::Hash, IntegrityError> {
    // Nonempty by the coinbase check. Compare real siblings BEFORE implicit
    // odd-leaf duplication, including at internal tree levels (CVE-2012-2459).
    while hashes.len() > 1 {
        let len = hashes.len();
        for pos in (0..len).step_by(2) {
            let left = hashes[pos];
            let right = hashes[(pos + 1).min(len - 1)];
            if pos + 1 < len && left == right {
                return Err(IntegrityError::MutatedMerkleTree);
            }
            let mut engine = sha256d::Hash::engine();
            engine.input(left.as_byte_array());
            engine.input(right.as_byte_array());
            hashes[pos / 2] = sha256d::Hash::from_engine(engine);
        }
        hashes.truncate(len.div_ceil(2));
    }
    Ok(hashes[0])
}

fn check_witness(block: &Block, rules: WitnessRules) -> Result<(), IntegrityError> {
    let coinbase = &block.txdata[0];
    if rules == WitnessRules::Active
        && let Some(output) = coinbase.output.iter().rfind(|output| {
            let script = output.script_pubkey.as_bytes();
            script.len() >= 38 && script.starts_with(&[0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed])
        })
    {
        let witness = &coinbase.input[0].witness;
        if witness.len() != 1 {
            return Err(IntegrityError::WitnessReservedValue);
        }
        let reserved = witness.iter().next().expect("one witness element");
        if reserved.len() != 32 {
            return Err(IntegrityError::WitnessReservedValue);
        }
        let root = block.witness_root().expect("nonempty block");
        let commitment = Block::compute_witness_commitment(&root, reserved);
        if commitment.as_byte_array() != &output.script_pubkey.as_bytes()[6..38] {
            return Err(IntegrityError::WitnessCommitment);
        }
        return Ok(());
    }
    if block
        .txdata
        .iter()
        .any(|tx| tx.input.iter().any(|input| !input.witness.is_empty()))
    {
        return Err(IntegrityError::UnexpectedWitness);
    }
    Ok(())
}
