//! Bounded current-membership reads using only the existing sole-reader pool.

use std::net::SocketAddr;
use std::time::Duration;

use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::Inventory;
use bitcoin::{Transaction, Txid};
use tokio::time::{Instant, timeout_at};

use crate::block_download_error::{BlockDownloadError, BlockPeerError};
use crate::header_wire::{Message, receive_transaction, send_bounded};
use crate::peer_connection::PeerConnection;
use crate::peer_pool::PeerPool;
use crate::sync_error::SyncError;
use crate::transaction_decode::decode_transaction;
use crate::transaction_submission_config::TransactionSubmissionConfig;
use crate::transaction_submission_report::TransactionSubmissionError;

/// Try at most two existing witness peers; never turn transport failure into absence.
pub(crate) async fn download(
    pool: &mut PeerPool,
    txid: Txid,
    preferred: Option<SocketAddr>,
) -> Result<Option<Transaction>, BlockDownloadError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = 1_000_000;
    let mut sent = 4096;
    let mut tried = Vec::with_capacity(2);
    let mut failure = None;
    let mut attempts = 0;
    for _ in 0..2 {
        let visit_deadline = deadline.min(Instant::now() + Duration::from_secs(2));
        let visit = async {
            let mut peer = match pool.acquire_transaction(preferred, &tried).await {
                Ok(Some(peer)) => peer,
                Ok(None) => return Ok(None),
                Err((addr, error)) => {
                    tried.extend(addr);
                    return Err(error);
                }
            };
            tried.push(peer.addr);
            attempts += 1;
            let result = request(&mut peer, txid, &mut received, &mut sent).await?;
            // Only a fully drained frame and checked identity can return a lease.
            pool.release(peer, false);
            Ok(result)
        };
        match timeout_at(visit_deadline, visit).await {
            Ok(Ok(Some(tx))) if Instant::now() <= visit_deadline => return Ok(Some(tx)),
            Ok(Ok(Some(_))) | Err(_) => failure = Some(SyncError::Timeout),
            Ok(Ok(None)) | Ok(Err(SyncError::TransactionLimit)) => {}
            Ok(Err(error)) => failure = Some(error),
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    match failure {
        Some(error) => Err(BlockDownloadError::Unavailable {
            attempts,
            last: BlockPeerError::Peer(error),
        }),
        None => Ok(None),
    }
}

/// Receive exactly the requested witness transaction under per-visit frame limits.
async fn request(
    peer: &mut PeerConnection,
    txid: Txid,
    received: &mut usize,
    sent: &mut usize,
) -> Result<Option<Transaction>, SyncError> {
    send_bounded(
        &mut peer.stream,
        NetworkMessage::GetData(vec![Inventory::WitnessTransaction(txid)]),
        sent,
    )
    .await?;
    for _ in 0..32 {
        match receive_transaction(&mut peer.stream, received, txid).await? {
            Message::Transaction(payload) => {
                let tx = decode_transaction(&payload, TransactionSubmissionConfig::default())
                    .map_err(|error| match error {
                        TransactionSubmissionError::Admission => SyncError::TransactionLimit,
                        _ => SyncError::Protocol("transaction encoding"),
                    })?;
                if tx.compute_txid() != txid {
                    return Err(SyncError::Protocol("transaction identity"));
                }
                return Ok(Some(tx));
            }
            Message::NotFound => return Ok(None),
            Message::Ping(nonce) => {
                send_bounded(&mut peer.stream, NetworkMessage::Pong(nonce), sent).await?;
            }
            Message::Ignore | Message::Pong(_) | Message::Addresses(_) => {}
            _ => return Err(SyncError::Protocol("unexpected transaction response")),
        }
    }
    Err(SyncError::Protocol("transaction response message budget"))
}

#[cfg(test)]
#[path = "transaction_download_tests.rs"]
mod tests;
