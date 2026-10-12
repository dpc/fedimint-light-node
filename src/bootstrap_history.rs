//! Forward P2P pagination into a fixed trusted checkpoint, never archival recovery.

use bitcoin::block::Header;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::GetHeadersMessage;
use tokio::time::timeout;

use crate::bootstrap_config::BootstrapConfig;
use crate::bootstrap_error::BootstrapError;
use crate::header_error::HeaderError;
use crate::header_wire::{Message, receive, send};
use crate::peer_handshake::handshake;
use crate::store_anchor::StoreAnchor;
use crate::sync_error::SyncError;

/// Retrieve exactly the bounded seed-to-checkpoint chain before trusting any row.
pub(crate) async fn history(
    config: &BootstrapConfig,
    anchor: StoreAnchor,
    start: u64,
    seed: Header,
) -> Result<Vec<Header>, BootstrapError> {
    let count = usize::try_from(anchor.height - start + 1)
        .map_err(|_| BootstrapError::Limit("history length"))?;
    let mut remaining_bytes = config.max_peer_bytes;
    let mut remaining_requests = config.max_header_requests;
    let mut last_error = BootstrapError::Reply("no history peer");
    for peer in &config.peers {
        let mut headers = Vec::with_capacity(count);
        headers.push(seed);
        let attempt = async {
            if count > 1 {
                let (mut stream, _) = timeout(
                    config.request_timeout,
                    handshake(*peer, 0, &mut remaining_bytes),
                )
                .await
                .map_err(|_| BootstrapError::Timeout)??;
                while headers.len() < count {
                    remaining_requests = remaining_requests
                        .checked_sub(1)
                        .ok_or(BootstrapError::Limit("header requests"))?;
                    let batch = timeout(config.request_timeout, async {
                        send(
                            &mut stream,
                            NetworkMessage::GetHeaders(GetHeadersMessage::new(
                                vec![headers.last().expect("seed").block_hash()],
                                anchor.hash,
                            )),
                        )
                        .await?;
                        for _ in 0..32 {
                            match receive(&mut stream, &mut remaining_bytes).await? {
                                Message::Headers(batch) => return Ok(batch),
                                Message::Ping(nonce) => {
                                    send(&mut stream, NetworkMessage::Pong(nonce)).await?
                                }
                                Message::Ignore => {}
                                _ => {
                                    return Err(SyncError::Protocol(
                                        "unexpected bootstrap message",
                                    ));
                                }
                            }
                        }
                        Err(SyncError::Protocol("bootstrap message count"))
                    })
                    .await
                    .map_err(|_| BootstrapError::Timeout)??;
                    if batch.is_empty() || batch.len() > count - headers.len() {
                        return Err(BootstrapError::Reply("incomplete or excessive history"));
                    }
                    for header in batch {
                        if header.prev_blockhash != headers.last().expect("seed").block_hash() {
                            return Err(HeaderError::Linkage.into());
                        }
                        headers.push(header);
                    }
                }
            }
            if headers.last().expect("seed").block_hash() != anchor.hash {
                return Err(HeaderError::Linkage.into());
            }
            Ok(())
        }
        .await;
        match attempt {
            Ok(()) => return Ok(headers),
            Err(error) => last_error = error,
        }
        if remaining_requests == 0 || remaining_bytes < 24 {
            break;
        }
    }
    Err(last_error)
}
