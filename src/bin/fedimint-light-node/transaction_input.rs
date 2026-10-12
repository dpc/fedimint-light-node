use std::fs::File;
use std::io::Read;
use std::path::Path;

use bitcoin::Transaction;
use fedimint_light_node::transaction_decode::decode_transaction;
use fedimint_light_node::transaction_submission_config::TransactionSubmissionConfig;

use crate::Result;

/// Refuse special files and bound reading even if the trusted regular file grows.
pub fn read(path: &Path, config: TransactionSubmissionConfig) -> Result<Transaction> {
    // As with database/export paths, the parent must not be attacker-writable.
    // The pre-open check avoids intentionally opening FIFOs/devices or symlinks.
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(
            "transaction input must be a regular file, not a symlink or special file".into(),
        );
    }
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > config.max_transaction_bytes as u64 {
        return Err("transaction input is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(config.max_transaction_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(decode_transaction(&bytes, config)?)
}
