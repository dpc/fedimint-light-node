//! Independent Core decisions for shared body checks, not consensus equivalence.

use bitcoin::BlockHash;
use bitcoin::hashes::{Hash, sha256};
use fedimint_light_node::block_decode::{BlockDecodeLimits, decode_block};
use fedimint_light_node::block_integrity::WitnessRules;
use fedimint_light_node::decode_error::DecodeError;
use fedimint_light_node::integrity_error::IntegrityError;

#[test]
fn core_regtest_merkle_and_witness_decisions_match_shared_checks() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("data/core-shared/manifest.json")).unwrap();
    let requested: BlockHash = manifest["hash"].as_str().unwrap().parse().unwrap();
    for (name, payload, expected) in [
        (
            "valid",
            include_bytes!("data/core-shared/valid.raw").as_slice(),
            None,
        ),
        (
            "bad-merkle",
            include_bytes!("data/core-shared/bad-merkle.raw").as_slice(),
            Some(IntegrityError::MerkleRoot),
        ),
        (
            "bad-witness",
            include_bytes!("data/core-shared/bad-witness.raw").as_slice(),
            Some(IntegrityError::WitnessCommitment),
        ),
    ] {
        assert_eq!(
            sha256::Hash::hash(payload).to_string(),
            manifest["vectors"][name]["sha256"].as_str().unwrap()
        );
        let result = decode_block(
            payload,
            requested,
            WitnessRules::Active,
            BlockDecodeLimits {
                max_payload_bytes: 4_000_000,
                max_elements: 1_000_000,
            },
        );
        match expected {
            None => {
                assert!(manifest["vectors"][name]["core_submitblock"].is_null());
                result.unwrap();
            }
            Some(expected) => {
                let core_reason = manifest["vectors"][name]["core_submitblock"]
                    .as_str()
                    .unwrap();
                assert_eq!(
                    core_reason,
                    if name == "bad-merkle" {
                        "bad-txnmrklroot"
                    } else {
                        "bad-witness-merkle-match"
                    }
                );
                assert!(
                    matches!(result, Err(DecodeError::Integrity(actual)) if actual == expected)
                );
            }
        }
    }
}
