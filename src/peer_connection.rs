use std::net::SocketAddr;

use bitcoin::p2p::ServiceFlags;
use tokio::net::TcpStream;

use crate::block_decode::BlockDecodeLimits;
use crate::header_wire::{Message, receive_observed};
use crate::sync_error::SyncError;

/// Exclusive clean-boundary lease; dropping it closes an interrupted exchange.
pub(crate) struct PeerConnection {
    /// Sole socket owner; never shared with an idle reader.
    pub stream: TcpStream,
    /// Actual connected address.
    pub addr: SocketAddr,
    /// Handshake claims, not validated capabilities.
    pub services: ServiceFlags,
    /// Remote protocol version and relay willingness for transaction submission.
    pub relay: bool,
    /// Coalesced discovery sample, bounded across every foreground exchange.
    pub learned: Vec<SocketAddr>,
    /// Whether this connection has already requested addresses.
    pub requested_addresses: bool,
    /// Idle rate window follows the connection across every lease.
    pub idle_window: tokio::time::Instant,
    /// Remaining idle bytes in that window, not reset by foreground service.
    pub idle_bytes: usize,
    /// Remaining idle frames in that window.
    pub idle_frames: u16,
    /// Optional inventory allowance and inbox, shared across lease transitions.
    pub observer: Option<crate::inventory_observer::InventoryObserver>,
}

impl PeerConnection {
    /// Receive under the caller's aggregate byte budget and retain at most 32 hints.
    pub async fn receive(
        &mut self,
        bytes: &mut usize,
        block: Option<BlockDecodeLimits>,
    ) -> Result<Message, SyncError> {
        match receive_observed(&mut self.stream, bytes, block, self.observer.as_mut()).await? {
            Message::Addresses(addresses) => {
                for addr in addresses {
                    if self.learned.len() == 32 {
                        break;
                    }
                    if !self.learned.contains(&addr) {
                        self.learned.push(addr);
                    }
                }
                Ok(Message::Ignore)
            }
            message => Ok(message),
        }
    }
}
