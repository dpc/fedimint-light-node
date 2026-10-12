//! Direct witness-aware `tx` writes; no inventory, mempool or retained retry state.
//!
//! Bitcoin Core's ProcessMessage(TX) explicitly permits unsolicited transactions
//! and decodes TX_WITH_WITNESS. Its TxDownloadManagerImpl::ReceivedTx does not
//! require an outstanding request. See docs/transaction-submission.md.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::time::Duration;

use bitcoin::consensus::{Encodable, serialize};
use bitcoin::hashes::{Hash, sha256d};
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::{Network, Transaction};
use tokio::io::AsyncWriteExt;
use tokio::time::{Instant, timeout_at};

use crate::encoded_size_limit::EncodedSizeLimit;
use crate::header_wire::{Message, send_bounded};
use crate::peer_connection::PeerConnection;
use crate::peer_pool::PeerPool;
use crate::sync_error::SyncError;
use crate::transaction_submission_config::TransactionSubmissionConfig;
use crate::transaction_submission_report::{
    SubmissionAttempt, SubmissionPeerError, TransactionSubmissionError, TransactionSubmissionReport,
};

/// Allocation-free bounded serialization precedes size arithmetic and hashing.
pub(crate) fn admit(
    tx: &Transaction,
    config: TransactionSubmissionConfig,
) -> Result<usize, TransactionSubmissionError> {
    if !config.validate() {
        return Err(TransactionSubmissionError::Configuration);
    }
    if tx.input.is_empty()
        || tx.output.is_empty()
        || tx.input.len() > config.max_transaction_bytes / 41
        || tx.output.len() > config.max_transaction_bytes / 9
    {
        return Err(TransactionSubmissionError::Admission);
    }
    let size = tx
        .consensus_encode(&mut bitcoin::io::FromStd::new(EncodedSizeLimit::new(
            config.max_transaction_bytes,
        )))
        .map_err(|_| TransactionSubmissionError::Admission)?;
    if tx.weight().to_wu() > config.max_weight {
        return Err(TransactionSubmissionError::Admission);
    }
    Ok(size)
}

/// Submit to each selected peer once, preserving every completed write observation.
pub(crate) async fn submit(
    tx: &Transaction,
    pool: &mut PeerPool,
    retry_delay: Duration,
    config: TransactionSubmissionConfig,
) -> Result<TransactionSubmissionReport, TransactionSubmissionError> {
    let size = admit(tx, config)?;
    let deadline = Instant::now() + config.total_timeout;
    // Serialize just once, without cloning the caller-owned transaction.
    let payload = serialize(tx);
    debug_assert_eq!(payload.len(), size);
    let mut envelope = [0u8; 24];
    envelope[..4].copy_from_slice(&Network::Bitcoin.magic().to_bytes());
    envelope[4..6].copy_from_slice(b"tx");
    envelope[16..20].copy_from_slice(&(size as u32).to_le_bytes());
    envelope[20..24].copy_from_slice(&sha256d::Hash::hash(&payload)[..4]);
    let mut report = TransactionSubmissionReport {
        txid: tx.compute_txid(),
        wtxid: tx.compute_wtxid(),
        attempts: Vec::with_capacity(config.max_attempts),
        received_bytes: 0,
        sent_bytes: 0,
    };
    let mut received = config.max_received_bytes;
    let mut sent = config.max_sent_bytes;
    let mut tried = Vec::new();
    for _ in 0..config.max_attempts.min(pool.attempt_limit()) {
        let mut peer = None;
        let mut started = false;
        let mut completed = false;
        // Pacing belongs to the total deadline, not the peer's visit deadline.
        let result = {
            let visit_deadline = deadline.min(Instant::now() + config.visit_timeout);
            let visit = async {
                let mut stream = pool
                    .acquire(
                        0,
                        retry_delay,
                        config.visit_timeout,
                        &mut received,
                        &mut sent,
                        true,
                    )
                    .await
                    .map_err(|(address, error)| {
                        peer = address;
                        error
                    })?;
                if tried.contains(&stream.addr) {
                    pool.release(stream, false);
                    return Err(SubmissionPeerError::Peer(SyncError::NoPeers));
                }
                peer = Some(stream.addr);
                tried.push(stream.addr);
                if !stream.relay || !stream.services.has(bitcoin::p2p::ServiceFlags::WITNESS) {
                    return Err(SyncError::Protocol(
                        "peer declines relay or lacks witness support",
                    )
                    .into());
                }
                sent = sent
                    .checked_sub(24 + payload.len())
                    .ok_or(SubmissionPeerError::SentBudget)?;
                // Timeout cannot preempt bounded synchronous work between awaits.
                if Instant::now() >= visit_deadline {
                    return Err(SubmissionPeerError::Timeout);
                }
                started = true;
                stream
                    .stream
                    .write_all(&envelope)
                    .await
                    .map_err(SyncError::from)?;
                stream
                    .stream
                    .write_all(&payload)
                    .await
                    .map_err(SyncError::from)?;
                // No acknowledgement exists: keep this observation even if time
                // expires immediately afterward. Never downgrade a completed write.
                completed = true;
                let drain_deadline = visit_deadline.min(Instant::now() + Duration::from_secs(2));
                let result =
                    timeout_at(drain_deadline, drain(&mut stream, &mut received, &mut sent))
                        .await
                        .map_err(|_| SubmissionPeerError::Timeout)?;
                if result.is_ok() {
                    pool.release(stream, false);
                }
                result
            };
            match timeout_at(visit_deadline, visit).await {
                Ok(result) => result,
                Err(_) => Err(SubmissionPeerError::Timeout),
            }
        };
        let Some(peer) = peer.or_else(|| pool.leased_address()) else {
            break;
        };
        let (result, post_write) = if completed {
            (Ok(()), Some(result))
        } else {
            (result, None)
        };
        report.attempts.push(SubmissionAttempt {
            peer,
            payload_write_started: started,
            result,
            post_write,
        });
        if Instant::now() >= deadline || received < 24 || sent < 24 {
            break;
        }
    }
    report.received_bytes = config.max_received_bytes - received;
    report.sent_bytes = config.max_sent_bytes - sent;
    Ok(report)
}

/// Drain queued control messages before closing to reduce unread-data TCP resets.
async fn drain(
    stream: &mut PeerConnection,
    received: &mut usize,
    sent: &mut usize,
) -> Result<(), SubmissionPeerError> {
    // A fresh keyed hash supplies a per-exchange nonce without a global RNG.
    // This is connection correlation, not cryptographic peer authentication.
    let nonce = RandomState::new().hash_one(std::time::Instant::now());
    send_bounded(&mut stream.stream, NetworkMessage::Ping(nonce), sent).await?;
    for _ in 0..32 {
        match stream.receive(received, None).await? {
            Message::Pong(reply) if reply == nonce => return Ok(()),
            Message::Ping(nonce) => {
                send_bounded(&mut stream.stream, NetworkMessage::Pong(nonce), sent).await?
            }
            Message::Ignore | Message::Pong(_) => {}
            _ => return Err(SyncError::Protocol("unexpected post-submission message").into()),
        }
    }
    Err(SyncError::Protocol("post-submission message budget").into())
}
