use bitcoin::blockdata::constants::genesis_block;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::Hash;
use bitcoin::{Block, BlockHash, Network};
use fedimint_light_node::block_decode::{BlockDecodeLimits, decode_block};
use fedimint_light_node::block_integrity::WitnessRules;
use fedimint_light_node::decode_error::DecodeError;
use fedimint_light_node::integrity_error::IntegrityError;

fn limits(bytes: usize, elements: usize) -> BlockDecodeLimits {
    BlockDecodeLimits {
        max_payload_bytes: bytes,
        max_elements: elements,
    }
}

fn reject(bytes: &[u8]) -> DecodeError {
    decode_block(
        bytes,
        BlockHash::all_zeros(),
        WitnessRules::Active,
        limits(4_000_000, 1_000_000),
    )
    .unwrap_err()
}

fn elements(block: &Block) -> usize {
    block.txdata.len()
        + block
            .txdata
            .iter()
            .map(|tx| {
                tx.input.len()
                    + tx.output.len()
                    + tx.input
                        .iter()
                        .map(|input| input.witness.len())
                        .sum::<usize>()
            })
            .sum::<usize>()
}

#[test]
fn real_legacy_and_witness_roundtrips_at_exact_budgets() {
    let legacy = serialize(&genesis_block(Network::Bitcoin));
    for bytes in [
        legacy.as_slice(),
        include_bytes!("data/testnet-segwit.raw").as_slice(),
    ] {
        let expected: Block = deserialize(bytes).unwrap();
        let budget = elements(&expected);
        let decoded = decode_block(
            bytes,
            expected.block_hash(),
            WitnessRules::Active,
            limits(bytes.len(), budget),
        )
        .unwrap();
        assert_eq!(decoded, expected);
        assert_eq!(serialize(&decoded), bytes);
        assert!(matches!(
            decode_block(
                bytes,
                expected.block_hash(),
                WitnessRules::Active,
                limits(bytes.len(), budget - 1)
            ),
            Err(DecodeError::ElementLimit)
        ));
        assert!(matches!(
            decode_block(
                bytes,
                expected.block_hash(),
                WitnessRules::Active,
                limits(bytes.len() - 1, budget)
            ),
            Err(DecodeError::PayloadLimit)
        ));
    }
}

#[test]
fn every_fixture_truncation_and_trailing_byte_is_rejected() {
    let bytes = include_bytes!("data/testnet-segwit.raw");
    for end in 0..bytes.len() {
        assert!(!matches!(reject(&bytes[..end]), DecodeError::Integrity(_)));
    }
    let mut trailing = bytes.to_vec();
    trailing.push(0);
    assert!(matches!(reject(&trailing), DecodeError::TrailingBytes));
}

#[test]
fn byte_limit_precedes_any_parsing_and_protocol_cap_is_not_relaxable() {
    assert!(matches!(
        decode_block(
            &[0],
            BlockHash::all_zeros(),
            WitnessRules::Active,
            limits(0, 0)
        ),
        Err(DecodeError::PayloadLimit)
    ));
    assert!(matches!(
        decode_block(
            &vec![0; 4_000_001],
            BlockHash::all_zeros(),
            WitnessRules::Active,
            limits(usize::MAX, usize::MAX)
        ),
        Err(DecodeError::PayloadLimit)
    ));
}

/// A structurally encoded transaction with one input, output and empty witness item.
/// The zero-valued header deliberately fails integrity, not decoding.
fn synthetic() -> Vec<u8> {
    let mut bytes = vec![0; 80];
    bytes.extend([1, 1, 0, 0, 0, 0, 1, 1]); // count, version, marker/flag, inputs
    bytes.extend([0; 36]); // outpoint
    bytes.push(0); // input script
    bytes.extend([0; 4]); // sequence
    bytes.push(1); // outputs
    bytes.extend([0; 8]); // value
    bytes.push(0); // output script
    bytes.extend([1, 0]); // witness items, empty item
    bytes.extend([0; 4]); // lock time
    bytes
}

#[test]
fn every_nested_length_rejects_nonminimal_and_huge_values() {
    let bytes = synthetic();
    // Block tx count, input count, scriptSig, output count, scriptPubKey,
    // witness stack count, and witness element length.
    for offset in [80, 87, 124, 129, 138, 139, 140] {
        let value = bytes[offset];
        let mut nonminimal = bytes[..offset].to_vec();
        nonminimal.extend([0xfd, value, 0]);
        nonminimal.extend_from_slice(&bytes[offset + 1..]);
        assert!(
            matches!(
                reject(&nonminimal),
                DecodeError::Encoding(bitcoin::consensus::encode::Error::NonMinimalVarInt)
            ),
            "offset {offset}"
        );
        let mut enormous = bytes[..offset].to_vec();
        enormous.extend([0xff; 9]);
        enormous.extend_from_slice(&bytes[offset + 1..]);
        assert!(
            matches!(
                reject(&enormous),
                DecodeError::ElementLimit | DecodeError::Truncated | DecodeError::LengthOverflow
            ),
            "offset {offset}"
        );
    }
}

#[test]
fn impossible_counts_fail_even_with_unlimited_structural_budget() {
    let mut bytes = vec![0; 80];
    bytes.extend([0xff; 9]);
    assert!(matches!(
        decode_block(
            &bytes,
            BlockHash::all_zeros(),
            WitnessRules::Active,
            limits(usize::MAX, usize::MAX)
        ),
        Err(DecodeError::Truncated | DecodeError::LengthOverflow)
    ));
}

#[test]
fn malformed_witness_flags_and_superfluous_witnesses_are_rejected() {
    for flag in [0, 2, 3, 255] {
        let mut bytes = synthetic();
        bytes[86] = flag;
        assert!(matches!(
            reject(&bytes),
            DecodeError::Encoding(bitcoin::consensus::encode::Error::UnsupportedSegwitFlag(f))
                if f == flag
        ));
    }
    let mut bytes = synthetic();
    bytes[139] = 0;
    bytes.remove(140);
    assert!(matches!(reject(&bytes), DecodeError::SuperfluousWitness));
}

#[test]
fn empty_witness_items_consume_the_aggregate_budget() {
    let mut bytes = synthetic();
    bytes.splice(139..141, [3, 0, 0, 0]);
    assert!(matches!(
        decode_block(
            &bytes,
            BlockHash::all_zeros(),
            WitnessRules::Active,
            limits(bytes.len(), 5) // tx + input + output + three witness items = 6
        ),
        Err(DecodeError::ElementLimit)
    ));
    assert!(matches!(
        decode_block(
            &bytes,
            BlockHash::all_zeros(),
            WitnessRules::Active,
            limits(bytes.len(), 6)
        ),
        Err(DecodeError::Integrity(_))
    ));
}

#[test]
fn decoded_bodies_feed_existing_integrity_and_activation_checks() {
    let bytes = include_bytes!("data/testnet-segwit.raw");
    assert!(matches!(
        reject(bytes),
        DecodeError::Integrity(IntegrityError::RequestedHash)
    ));
    let block: Block = deserialize(bytes).unwrap();
    assert!(matches!(
        decode_block(
            bytes,
            block.block_hash(),
            WitnessRules::BeforeActivation,
            limits(bytes.len(), elements(&block))
        ),
        Err(DecodeError::Integrity(IntegrityError::UnexpectedWitness))
    ));
    let mut mutated = block;
    mutated.txdata[0].output[0].value = bitcoin::Amount::ZERO;
    assert!(matches!(
        decode_block(
            &serialize(&mutated),
            mutated.block_hash(),
            WitnessRules::Active,
            limits(bytes.len(), elements(&mutated))
        ),
        Err(DecodeError::Integrity(IntegrityError::MerkleRoot))
    ));
}
