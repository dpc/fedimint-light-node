#![no_main]

use bitcoin::BlockHash;
use bitcoin::block::Header;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::Hash;
use fedimint_light_node::block_decode::{BlockDecodeLimits, decode_block};
use fedimint_light_node::block_integrity::WitnessRules;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let hash = bytes
        .get(..80)
        .and_then(|bytes| deserialize::<Header>(bytes).ok())
        .map(|header| header.block_hash())
        .unwrap_or_else(BlockHash::all_zeros);
    for witness in [WitnessRules::Active, WitnessRules::BeforeActivation] {
        if let Ok(block) = decode_block(
            bytes,
            hash,
            witness,
            BlockDecodeLimits {
                max_payload_bytes: 4_000_000,
                max_elements: 1_000_000,
            },
        ) {
            assert_eq!(serialize(&block), bytes);
            assert_eq!(block.block_hash(), hash);
        }
    }
});
