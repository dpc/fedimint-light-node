use bitcoin::Transaction;
use bitcoin::consensus::deserialize;

use crate::block_decode::preflight_transaction;
use crate::decode_error::DecodeError;
use crate::transaction_submission::admit;
use crate::transaction_submission_config::TransactionSubmissionConfig;
use crate::transaction_submission_report::TransactionSubmissionError;

/// Decode a bounded canonical raw transaction, never hex text or a PSBT.
///
/// An allocation-free structural scan rejects nonminimal lengths, truncation,
/// unsupported/superfluous witness encodings and trailing data before decoding.
/// Aggregate decoded elements cannot exceed the byte limit (at most 400,000).
/// Byte and weight admission is not spend, script, fee or standardness validation.
/// The input and returned allocations belong to the caller; no RSS bound is implied.
pub fn decode_transaction(
    payload: &[u8],
    config: TransactionSubmissionConfig,
) -> Result<Transaction, TransactionSubmissionError> {
    if !config.validate() {
        return Err(TransactionSubmissionError::Configuration);
    }
    if payload.len() > config.max_transaction_bytes {
        return Err(TransactionSubmissionError::Admission);
    }
    let mut remaining = payload;
    let mut elements = config.max_transaction_bytes;
    preflight_transaction(&mut remaining, &mut elements)?;
    if !remaining.is_empty() {
        return Err(DecodeError::TrailingBytes.into());
    }
    let tx: Transaction = deserialize(payload).map_err(DecodeError::from)?;
    admit(&tx, config)?;
    Ok(tx)
}
