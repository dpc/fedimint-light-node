//! Shared bounded mainnet v1 handshake; the caller owns deadlines and byte budgets.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};

use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_network::VersionMessage;
use tokio::net::TcpStream;

use crate::header_wire::{Message, receive, send_bounded};
use crate::sync_error::SyncError;

/// Connect and negotiate without trusting the remote advertised height.
pub(crate) async fn handshake(
    peer: SocketAddr,
    height: u64,
    bytes: &mut usize,
) -> Result<(TcpStream, ServiceFlags), SyncError> {
    let mut sent = usize::MAX;
    let (stream, version) = handshake_inner(peer, height, bytes, &mut sent, false).await?;
    Ok((stream, version.services))
}

/// Require a witness-aware transaction-receiving peer and budget every control write.
pub(crate) async fn submission_handshake(
    peer: SocketAddr,
    bytes: &mut usize,
    sent: &mut usize,
) -> Result<TcpStream, SyncError> {
    let (stream, _) = handshake_inner(peer, 0, bytes, sent, true).await?;
    Ok(stream)
}

/// Share the bounded handshake without changing header/body peer policy.
async fn handshake_inner(
    peer: SocketAddr,
    height: u64,
    bytes: &mut usize,
    sent: &mut usize,
    submission: bool,
) -> Result<(TcpStream, VersionMessage), SyncError> {
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
    if submission {
        // rust-bitcoin's constructor still defaults to 70001. Advertise the
        // witness-era protocol we actually use, without opting into wtxid relay.
        version.version = 70012;
    }
    send_bounded(&mut stream, NetworkMessage::Version(version), sent).await?;
    let mut remote = None;
    for _ in 0..32 {
        match receive(&mut stream, bytes).await? {
            Message::Version(version) if remote.is_none() => {
                if version.version < 70001
                    || !(version.services.has(ServiceFlags::NETWORK)
                        || version.services.has(ServiceFlags::NETWORK_LIMITED))
                {
                    return Err(SyncError::Protocol("unsupported peer version or services"));
                }
                if submission
                    && (version.version < 70012
                        || !version.relay
                        || !version.services.has(ServiceFlags::WITNESS))
                {
                    return Err(SyncError::Protocol(
                        "peer declines relay or lacks witness support",
                    ));
                }
                remote = Some(version);
                send_bounded(&mut stream, NetworkMessage::Verack, sent).await?;
            }
            Message::Verack if remote.is_some() => {
                return Ok((stream, remote.expect("version received")));
            }
            Message::Ping(nonce) => {
                send_bounded(&mut stream, NetworkMessage::Pong(nonce), sent).await?
            }
            Message::Ignore | Message::Pong(_) => {}
            _ => return Err(SyncError::Protocol("unexpected handshake message")),
        }
    }
    Err(SyncError::Protocol("handshake message budget"))
}
