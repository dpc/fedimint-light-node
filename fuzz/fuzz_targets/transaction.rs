#![no_main]

use bitcoin::consensus::serialize;
use fedimint_light_node::transaction_decode::decode_transaction;
use fedimint_light_node::transaction_submission_config::TransactionSubmissionConfig;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    if let Ok(transaction) = decode_transaction(bytes, TransactionSubmissionConfig::default()) {
        assert_eq!(serialize(&transaction), bytes);
    }
});
