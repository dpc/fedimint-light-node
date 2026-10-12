use bitcoin::blockdata::constants::genesis_block;
use bitcoin::hashes::Hash;
use bitcoin::{
    Amount, Block, BlockHash, Network, OutPoint, ScriptBuf, Transaction, TxOut, Txid, Witness,
};
use fedimint_light_node::block_integrity::{WitnessRules, check_block_integrity};
use fedimint_light_node::integrity_error::IntegrityError;

#[test]
fn real_testnet_witness_block_matches_known_hash_and_commitments() {
    // Fixed 4,319-byte fixture, not an untrusted decoding interface.
    let block: Block =
        bitcoin::consensus::deserialize(include_bytes!("data/testnet-segwit.raw")).unwrap();
    let requested = "000000000000045e0b1660b6445b5e5c5ab63c9a4f956be7e1e69be04fa4497b"
        .parse()
        .unwrap();
    assert_eq!(block.total_size(), 4_319);
    assert_eq!(block.weight().to_wu(), 17_168);
    assert_eq!(
        check_block_integrity(&block, requested, WitnessRules::Active, 4_319),
        Ok(())
    );
    assert_eq!(
        fedimint_light_node::declared_pow::check_declared_pow(
            &block.header,
            bitcoin::Target::MAX_ATTAINABLE_TESTNET,
        ),
        Ok(())
    );
}

fn legacy() -> Block {
    genesis_block(Network::Bitcoin)
}

fn refresh(block: &mut Block) {
    block.header.merkle_root = block.compute_merkle_root().unwrap();
}

fn check(block: &Block) -> Result<(), IntegrityError> {
    check_block_integrity(block, block.block_hash(), WitnessRules::Active, 4_000_000)
}

fn ordinary(tag: u8) -> Transaction {
    let mut tx = legacy().txdata.remove(0);
    tx.input[0].previous_output = OutPoint::new(Txid::from_byte_array([tag; 32]), 0);
    tx
}

fn with_transaction() -> Block {
    let mut block = legacy();
    block.txdata.push(ordinary(1));
    refresh(&mut block);
    block
}

fn commitment_script(block: &Block, reserved: &[u8]) -> ScriptBuf {
    let root = block.witness_root().unwrap();
    let commitment = Block::compute_witness_commitment(&root, reserved);
    let mut bytes = vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
    bytes.extend_from_slice(commitment.as_byte_array());
    ScriptBuf::from_bytes(bytes)
}

fn witness_block() -> Block {
    let mut block = with_transaction();
    block.txdata[1].input[0].witness = Witness::from_slice(&[b"arbitrary witness"]);
    block.txdata[0].input[0].witness = Witness::from_slice(&[[42; 32]]);
    let script_pubkey = commitment_script(&block, &[42; 32]);
    block.txdata[0].output.push(TxOut {
        value: Amount::ZERO,
        script_pubkey,
    });
    refresh(&mut block);
    block
}

#[test]
fn genesis_and_odd_and_even_transaction_trees_pass() {
    let mut block = legacy();
    for tag in 1..9 {
        assert_eq!(check(&block), Ok(()));
        block.txdata.push(ordinary(tag));
        refresh(&mut block);
    }
}

#[test]
fn requested_hash_is_not_taken_from_the_peer() {
    assert_eq!(
        check_block_integrity(
            &legacy(),
            BlockHash::all_zeros(),
            WitnessRules::Active,
            4_000_000
        ),
        Err(IntegrityError::RequestedHash)
    );
}

#[test]
fn altered_body_does_not_match_requested_header() {
    let mut block = legacy();
    block.txdata[0].output[0].value = Amount::ZERO;
    assert_eq!(check(&block), Err(IntegrityError::MerkleRoot));
}

#[test]
fn empty_block_and_missing_misplaced_or_multiple_coinbases_fail() {
    let mut block = legacy();
    block.txdata.clear();
    assert_eq!(check(&block), Err(IntegrityError::Coinbase));
    block.txdata.push(ordinary(1));
    assert_eq!(check(&block), Err(IntegrityError::Coinbase));
    block.txdata.push(legacy().txdata.remove(0));
    assert_eq!(check(&block), Err(IntegrityError::Coinbase));
    block = legacy();
    block.txdata.push(block.txdata[0].clone());
    assert_eq!(check(&block), Err(IntegrityError::Coinbase));
}

#[test]
fn empty_transaction_inputs_or_outputs_fail() {
    let mut block = with_transaction();
    block.txdata[1].input.clear();
    assert_eq!(check(&block), Err(IntegrityError::EmptyTransaction));
    block = with_transaction();
    block.txdata[1].output.clear();
    assert_eq!(check(&block), Err(IntegrityError::EmptyTransaction));
}

#[test]
fn coinbase_script_length_boundaries() {
    for len in [0, 1, 2, 100, 101] {
        let mut block = legacy();
        block.txdata[0].input[0].script_sig = ScriptBuf::from_bytes(vec![0; len]);
        refresh(&mut block);
        assert_eq!(
            check(&block),
            if (2..=100).contains(&len) {
                Ok(())
            } else {
                Err(IntegrityError::CoinbaseScript)
            }
        );
    }
}

#[test]
fn null_input_outside_coinbase_fails() {
    let mut block = with_transaction();
    let mut input = block.txdata[1].input[0].clone();
    input.previous_output = OutPoint::null();
    block.txdata[1].input.push(input);
    assert_eq!(check(&block), Err(IntegrityError::NullInput));
}

#[test]
fn repeated_input_fails_even_with_different_script_and_witness() {
    let mut block = with_transaction();
    let mut input = block.txdata[1].input[0].clone();
    input.script_sig = ScriptBuf::new();
    input.witness = Witness::from_slice(&[b"different"]);
    block.txdata[1].input.push(input);
    assert_eq!(check(&block), Err(IntegrityError::DuplicateInput));
}

#[test]
fn output_money_ranges_are_per_transaction_not_per_block() {
    let mut block = with_transaction();
    for tx in &mut block.txdata {
        tx.output[0].value = Amount::MAX_MONEY;
    }
    refresh(&mut block);
    assert_eq!(check(&block), Ok(()));
    for value in [Amount::MAX_MONEY.to_sat() + 1, u64::MAX] {
        block.txdata[1].output[0].value = Amount::from_sat(value);
        assert_eq!(check(&block), Err(IntegrityError::OutputValue));
    }
    block.txdata[1].output[0].value = Amount::MAX_MONEY;
    let mut extra = block.txdata[1].output[0].clone();
    extra.value = Amount::from_sat(1);
    block.txdata[1].output.push(extra);
    assert_eq!(check(&block), Err(IntegrityError::OutputValue));
}

#[test]
fn odd_leaf_duplication_preserves_hash_but_is_rejected() {
    let mut block = with_transaction();
    block.txdata.push(ordinary(2));
    refresh(&mut block);
    let requested = block.block_hash();
    block.txdata.push(block.txdata[2].clone());
    assert_eq!(block.compute_merkle_root(), Some(block.header.merkle_root));
    assert_eq!(
        check_block_integrity(&block, requested, WitnessRules::Active, 4_000_000),
        Err(IntegrityError::DuplicateTransaction)
    );
}

#[test]
fn nonadjacent_duplicate_txids_with_distinct_witnesses_fail() {
    let mut block = with_transaction();
    block.txdata.push(ordinary(2));
    let mut duplicate = block.txdata[1].clone();
    duplicate.input[0].witness = Witness::from_slice(&[b"different"]);
    block.txdata.push(duplicate);
    refresh(&mut block);
    assert_eq!(check(&block), Err(IntegrityError::DuplicateTransaction));
}

#[test]
fn witness_commitment_passes_and_authenticates_witness_only_changes() {
    let mut block = witness_block();
    assert_eq!(check(&block), Ok(()));
    let original_hash = block.block_hash();
    block.txdata[1].input[0].witness = Witness::from_slice(&[b"corrupt"]);
    assert_eq!(block.block_hash(), original_hash);
    assert_eq!(check(&block), Err(IntegrityError::WitnessCommitment));
}

#[test]
fn highest_matching_output_wins_and_trailing_data_is_allowed() {
    let mut block = witness_block();
    let valid = block.txdata[0].output.last().unwrap().clone();
    let mut invalid = valid.clone();
    let mut bytes = invalid.script_pubkey.into_bytes();
    bytes[10] ^= 1;
    invalid.script_pubkey = ScriptBuf::from_bytes(bytes);
    block.txdata[0].output.push(invalid);
    refresh(&mut block);
    assert_eq!(check(&block), Err(IntegrityError::WitnessCommitment));
    let mut valid = valid;
    let mut bytes = valid.script_pubkey.into_bytes();
    bytes.extend_from_slice(b"ignored trailing bytes");
    valid.script_pubkey = ScriptBuf::from_bytes(bytes);
    block.txdata[0].output.push(valid);
    refresh(&mut block);
    assert_eq!(check(&block), Ok(()));
}

#[test]
fn present_commitment_requires_reserved_value_even_without_any_witness() {
    let mut block = witness_block();
    for tx in &mut block.txdata {
        tx.input[0].witness = Witness::new();
    }
    // Regression: dependency's convenience method returns true here.
    assert!(block.check_witness_commitment());
    assert_eq!(check(&block), Err(IntegrityError::WitnessReservedValue));
}

#[test]
fn reserved_value_must_be_exactly_one_32_byte_element() {
    for witness in [
        Witness::new(),
        Witness::from_slice(&[vec![0; 31]]),
        Witness::from_slice(&[vec![0; 33]]),
        Witness::from_slice(&[vec![0; 32], vec![]]),
    ] {
        let mut block = witness_block();
        block.txdata[0].input[0].witness = witness;
        assert_eq!(check(&block), Err(IntegrityError::WitnessReservedValue));
    }
    let mut block = witness_block();
    block.txdata[0].input[0].witness = Witness::from_slice(&[[43; 32]]);
    assert_eq!(check(&block), Err(IntegrityError::WitnessCommitment));
}

#[test]
fn no_commitment_forbids_witness_including_coinbase_only() {
    for index in [0, 1] {
        let mut block = with_transaction();
        block.txdata[index].input[0].witness = Witness::from_slice(&[b""]);
        assert_eq!(check(&block), Err(IntegrityError::UnexpectedWitness));
    }
}

#[test]
fn short_or_wrong_magic_output_is_not_a_commitment() {
    for short in [true, false] {
        let mut block = witness_block();
        let output = block.txdata[0].output.last_mut().unwrap();
        let mut bytes = output.script_pubkey.clone().into_bytes();
        if short {
            bytes.truncate(37);
        } else {
            bytes[5] ^= 1;
        }
        output.script_pubkey = ScriptBuf::from_bytes(bytes);
        refresh(&mut block);
        assert_eq!(check(&block), Err(IntegrityError::UnexpectedWitness));
    }
}

#[test]
fn activation_is_explicit_and_preactivation_ignores_commitment_pattern() {
    let mut block = witness_block();
    assert_eq!(
        check_block_integrity(
            &block,
            block.block_hash(),
            WitnessRules::BeforeActivation,
            4_000_000
        ),
        Err(IntegrityError::UnexpectedWitness)
    );
    for tx in &mut block.txdata {
        tx.input[0].witness = Witness::new();
    }
    assert_eq!(
        check_block_integrity(
            &block,
            block.block_hash(),
            WitnessRules::BeforeActivation,
            4_000_000
        ),
        Ok(())
    );
    assert_eq!(check(&block), Err(IntegrityError::WitnessReservedValue));
}

#[test]
fn witness_commitment_can_cover_only_coinbase() {
    let mut block = legacy();
    block.txdata[0].input[0].witness = Witness::from_slice(&[[0; 32]]);
    let script_pubkey = commitment_script(&block, &[0; 32]);
    block.txdata[0].output.push(TxOut {
        value: Amount::ZERO,
        script_pubkey,
    });
    refresh(&mut block);
    assert_eq!(check(&block), Ok(()));
}

#[test]
fn caller_byte_budget_is_inclusive_and_cannot_relax_protocol_limit() {
    let block = witness_block();
    for limit in [0, block.total_size() - 1, block.total_size(), usize::MAX] {
        assert_eq!(
            check_block_integrity(&block, block.block_hash(), WitnessRules::Active, limit),
            if limit < block.total_size() {
                Err(IntegrityError::SerializedSize)
            } else {
                Ok(())
            }
        );
    }
    let mut block = legacy();
    block.txdata[0].output[0].script_pubkey = ScriptBuf::from_bytes(vec![0; 4_000_000]);
    assert_eq!(
        check_block_integrity(&block, block.block_hash(), WitnessRules::Active, usize::MAX),
        Err(IntegrityError::SerializedSize)
    );
}

#[test]
fn stripped_size_weight_boundary_is_inclusive() {
    let mut block = legacy();
    block.txdata[0].output[0].script_pubkey = ScriptBuf::from_bytes(vec![0; 999_000]);
    let remaining = 1_000_000 - block.total_size();
    block.txdata[0].output[0].script_pubkey = ScriptBuf::from_bytes(vec![0; 999_000 + remaining]);
    assert_eq!(block.weight().to_wu(), 4_000_000);
    refresh(&mut block);
    assert_eq!(check(&block), Ok(()));
    block.txdata[0].output[0]
        .script_pubkey
        .push_opcode(bitcoin::opcodes::OP_FALSE);
    assert_eq!(check(&block), Err(IntegrityError::Weight));
}

#[test]
fn witness_weight_boundary_is_inclusive() {
    let mut block = witness_block();
    block.txdata[1].input[0].witness = Witness::from_slice(&[vec![0; 3_990_000]]);
    let remaining = 4_000_000 - block.weight().to_wu() as usize;
    block.txdata[1].input[0].witness = Witness::from_slice(&[vec![0; 3_990_000 + remaining]]);
    block.txdata[0].output[1].script_pubkey = commitment_script(&block, &[42; 32]);
    refresh(&mut block);
    assert_eq!(block.weight().to_wu(), 4_000_000);
    assert_eq!(check(&block), Ok(()));
    block.txdata[1].input[0].witness = Witness::from_slice(&[vec![0; 3_990_001 + remaining]]);
    assert_eq!(check(&block), Err(IntegrityError::Weight));
}

#[test]
fn invalid_spends_and_no_pow_can_pass_body_integrity() {
    let mut block = with_transaction();
    // Nonexistent outpoint, no meaningful scripts, inflated coinbase, arbitrary
    // timestamp/difficulty: deliberately outside this mechanical boundary.
    block.txdata[0].output[0].value = Amount::MAX_MONEY;
    block.txdata[1].input[0].script_sig = ScriptBuf::new();
    block.header.bits = bitcoin::CompactTarget::from_consensus(0);
    block.header.time = 0;
    refresh(&mut block);
    assert_eq!(check(&block), Ok(()));
}
