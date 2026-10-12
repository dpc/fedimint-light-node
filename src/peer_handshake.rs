//! Shared bounded mainnet v1 handshake; the caller owns deadlines and byte budgets.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};

use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_network::VersionMessage;
use tokio::net::TcpStream;

use crate::header_wire::{Message, receive, send};
use crate::sync_error::SyncError;

/// Connect and negotiate without trusting the remote advertised height.
pub(crate) async fn handshake(
    peer: SocketAddr,
    height: u64,
    bytes: &mut usize,
) -> Result<(TcpStream, ServiceFlags), SyncError> {
    let mut stream = TcpStream::connect(peer).await?;
    let mut version = VersionMessage::new(
        ServiceFlags::NONE,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        Address::new(&peer, ServiceFlags::NONE),
        Address::new(
            &SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            ServiceFlags::NONE,
        ),
        0, // No listener: no self-connection detection is needed.
        "/fedimint-light-node:0.1/".into(),
        height.min(i32::MAX as u64) as i32,
    );
    version.relay = false;
    send(&mut stream, NetworkMessage::Version(version)).await?;
    let mut services = None;
    for _ in 0..32 {
        match receive(&mut stream, bytes).await? {
            Message::Version(version) if services.is_none() => {
                if version.version < 70001
                    || !(version.services.has(ServiceFlags::NETWORK)
                        || version.services.has(ServiceFlags::NETWORK_LIMITED))
                {
                    return Err(SyncError::Protocol("unsupported peer version or services"));
                }
                services = Some(version.services);
                send(&mut stream, NetworkMessage::Verack).await?;
            }
            Message::Verack if services.is_some() => {
                return Ok((stream, services.expect("version received")));
            }
            Message::Ping(nonce) => send(&mut stream, NetworkMessage::Pong(nonce)).await?,
            Message::Ignore => {}
            _ => return Err(SyncError::Protocol("unexpected handshake message")),
        }
    }
    Err(SyncError::Protocol("handshake message budget"))
}
