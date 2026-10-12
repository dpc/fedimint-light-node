//! Sequential, task-free body retrieval. `HeaderSync` owns selection and admission.

use std::net::SocketAddr;
use std::time::Duration;

use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::Inventory;
use bitcoin::{Block, BlockHash};
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep, timeout_at};

use crate::block_download_config::BlockDownloadConfig;
use crate::block_download_error::{BlockDownloadError, BlockPeerError};
use crate::block_integrity::WitnessRules;
use crate::header_wire::{Message, receive_inner, send};
use crate::peer_handshake::handshake;
use crate::sync_error::SyncError;

/// Mainnet buried SegWit height, from Bitcoin Core v30.0 chain parameters.
pub(crate) fn witness_rules(height: u64) -> WitnessRules {
    if height >= 481_824 {
        WitnessRules::Active
    } else {
        WitnessRules::BeforeActivation
    }
}

/// Visit each configured entry at most once, under one aggregate byte budget.
pub(crate) async fn download(
    peers: &[SocketAddr],
    next_peer: &mut usize,
    retry_delay: Duration,
    tip_height: u64,
    height: u64,
    hash: BlockHash,
    config: BlockDownloadConfig,
) -> Result<Block, BlockDownloadError> {
    let deadline = Instant::now() + config.total_timeout;
    let mut remaining_bytes = config.max_received_bytes;
    let mut last = BlockPeerError::ByteBudget;
    let mut attempts = 0;
    for _ in 0..config.max_attempts.min(peers.len()) {
        if remaining_bytes < 24 {
            last = BlockPeerError::ByteBudget;
            break;
        }
        // Reserve rotation before pacing too, matching cancelled header visits.
        let peer = peers[*next_peer];
        *next_peer = (*next_peer + 1) % peers.len();
        if timeout_at(deadline, sleep(retry_delay)).await.is_err() {
            last = BlockPeerError::Timeout;
            break;
        }
        attempts += 1;
        let visit_deadline = deadline.min(Instant::now() + config.visit_timeout);
        let visit = async {
            let (mut stream, services) = handshake(peer, tip_height, &mut remaining_bytes).await?;
            // BIP159 promises only the peer's recent 288 blocks. Our selected tip
            // and its advertised height cannot prove proximity to its real tip.
            // Conservatively require full history even for locally recent blocks.
            if !services.has(ServiceFlags::NETWORK | ServiceFlags::WITNESS) {
                return Err(BlockPeerError::Services);
            }
            request_block(
                &mut stream,
                hash,
                witness_rules(height),
                &mut remaining_bytes,
                config,
            )
            .await
        };
        match timeout_at(visit_deadline, visit).await {
            Ok(Ok(block)) => {
                // Timeout futures cannot preempt synchronous decode/checking.
                // Do not deliver success if that bounded work overran the deadline.
                if Instant::now() <= visit_deadline {
                    return Ok(block);
                }
                last = BlockPeerError::Timeout;
            }
            Ok(Err(error)) => last = error,
            Err(_) => last = BlockPeerError::Timeout,
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    Err(BlockDownloadError::Unavailable { attempts, last })
}

/// Request witness serialization even for pre-activation blocks (BIP144).
async fn request_block(
    stream: &mut TcpStream,
    hash: BlockHash,
    rules: WitnessRules,
    remaining_bytes: &mut usize,
    config: BlockDownloadConfig,
) -> Result<Block, BlockPeerError> {
    send(
        stream,
        NetworkMessage::GetData(vec![Inventory::WitnessBlock(hash)]),
    )
    .await?;
    for _ in 0..32 {
        match receive_inner(stream, remaining_bytes, Some(config.decode)).await? {
            Message::Block(frame, payload) => return Ok(frame.decode(&payload, hash, rules)?),
            Message::NotFound => return Err(BlockPeerError::NotFound),
            Message::Ping(nonce) => send(stream, NetworkMessage::Pong(nonce)).await?,
            Message::Ignore | Message::Pong(_) => {}
            _ => return Err(SyncError::Protocol("unexpected block response").into()),
        }
    }
    Err(SyncError::Protocol("block response message budget").into())
}

#[cfg(test)]
#[path = "block_download_tests.rs"]
mod tests;
