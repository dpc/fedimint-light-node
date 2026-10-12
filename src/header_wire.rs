//! Narrow v1 transport: rust-bitcoin encodes outgoing messages and fixed headers.
//! The broad RawNetworkMessage decoder is deliberately not used on hostile input:
//! count/length preflight precedes all variable allocation. No body commands pass.

use bitcoin::Network;
use bitcoin::block::Header;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{Hash, HashEngine, sha256d};
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_network::VersionMessage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::sync_error::SyncError;

/// Maximum protocol header batch, independent of local fork capacity.
pub(crate) const MAX_HEADERS: usize = 2000;

/// A narrowly admitted frame; auxiliary commands never allocate a payload.
pub(crate) enum Message {
    /// Structurally bounded peer version.
    Version(VersionMessage),
    /// Peer handshake completion.
    Verack,
    /// Bounded header batch.
    Headers(Vec<Header>),
    /// Keepalive challenge.
    Ping(u64),
    /// Bounded, uninterpreted protocol chatter.
    Ignore,
}

/// Send one locally constructed message; no peer-sized outbound allocation.
pub(crate) async fn send(stream: &mut TcpStream, message: NetworkMessage) -> Result<(), SyncError> {
    stream
        .write_all(&serialize(&RawNetworkMessage::new(
            Network::Bitcoin.magic(),
            message,
        )))
        .await?;
    Ok(())
}

/// Read one admitted frame with at most 162,003 allocated payload bytes.
///
/// Ignored payloads use a fixed 4 KiB buffer; total bytes and message count are
/// bounded by the enclosing exchange. Rejection closes the connection, never scans
/// for a new magic. Checksums are corruption detection, not peer authentication.
pub(crate) async fn receive(
    stream: &mut TcpStream,
    remaining_bytes: &mut usize,
) -> Result<Message, SyncError> {
    let mut envelope = [0; 24];
    stream.read_exact(&mut envelope).await?;
    if envelope[..4] != Network::Bitcoin.magic().to_bytes() {
        return Err(SyncError::Protocol("network magic"));
    }
    let command = &envelope[4..16];
    let (limit, buffered) = match command {
        b"version\0\0\0\0\0" => (350, true),
        b"headers\0\0\0\0\0" => (3 + MAX_HEADERS * 81, true),
        b"ping\0\0\0\0\0\0\0\0" => (8, true),
        b"verack\0\0\0\0\0\0" | b"sendheaders\0" | b"sendaddrv2\0\0" | b"wtxidrelay\0\0" => {
            (0, false)
        }
        b"pong\0\0\0\0\0\0\0\0" | b"feefilter\0\0\0" => (8, false),
        b"sendcmpct\0\0\0" => (9, false),
        b"inv\0\0\0\0\0\0\0\0\0" | b"notfound\0\0\0\0" => (1_800_003, false),
        b"addr\0\0\0\0\0\0\0\0" => (30_003, false),
        _ => return Err(SyncError::Protocol("unsupported command")),
    };
    let len = u32::from_le_bytes(envelope[16..20].try_into().expect("fixed slice")) as usize;
    if len > limit || len + 24 > *remaining_bytes {
        return Err(SyncError::Protocol("frame or exchange byte budget"));
    }
    *remaining_bytes -= len + 24;
    let mut engine = sha256d::Hash::engine();
    let payload = if buffered {
        let mut payload = vec![0; len];
        stream.read_exact(&mut payload).await?;
        engine.input(&payload);
        payload
    } else {
        let mut scratch = [0; 4096];
        let mut remaining = len;
        while remaining != 0 {
            let count = remaining.min(scratch.len());
            stream.read_exact(&mut scratch[..count]).await?;
            engine.input(&scratch[..count]);
            remaining -= count;
        }
        Vec::new()
    };
    if sha256d::Hash::from_engine(engine)[..4] != envelope[20..24] {
        return Err(SyncError::Protocol("checksum"));
    }
    match command {
        b"version\0\0\0\0\0" => Ok(Message::Version(version(&payload)?)),
        b"headers\0\0\0\0\0" => Ok(Message::Headers(headers(&payload)?)),
        b"verack\0\0\0\0\0\0" => Ok(Message::Verack),
        b"ping\0\0\0\0\0\0\0\0" => Ok(Message::Ping(u64::from_le_bytes(
            payload
                .try_into()
                .map_err(|_| SyncError::Protocol("ping length"))?,
        ))),
        _ => Ok(Message::Ignore),
    }
}

/// Parse canonical CompactSize without any allocation.
fn count(bytes: &[u8]) -> Result<(usize, usize), SyncError> {
    match bytes {
        [n @ 0..=252, ..] => Ok((*n as usize, 1)),
        [253, a, b, ..] => {
            let n = u16::from_le_bytes([*a, *b]) as usize;
            if n >= 253 {
                Ok((n, 3))
            } else {
                Err(SyncError::Protocol("noncanonical count"))
            }
        }
        _ => Err(SyncError::Protocol("excessive or truncated count")),
    }
}

/// Preflight vector length, exact shape, and all zero transaction counts first.
fn headers(payload: &[u8]) -> Result<Vec<Header>, SyncError> {
    let (len, prefix) = count(payload)?;
    if len > MAX_HEADERS || payload.len() != prefix + len * 81 {
        return Err(SyncError::Protocol("header count or length"));
    }
    let chunks = payload[prefix..].as_chunks::<81>().0.iter();
    if chunks.clone().any(|header| header[80] != 0) {
        return Err(SyncError::Protocol("nonzero header transaction count"));
    }
    chunks
        .map(|header| {
            deserialize(&header[..80]).map_err(|_| SyncError::Protocol("header encoding"))
        })
        .collect()
}

/// Version's user agent is the sole variable field; bound it before Rust decoding.
fn version(payload: &[u8]) -> Result<VersionMessage, SyncError> {
    let tail = payload
        .get(80..)
        .ok_or(SyncError::Protocol("short version"))?;
    let (len, prefix) = count(tail)?;
    if len > 256 || !(prefix + len + 4..=prefix + len + 5).contains(&tail.len()) {
        return Err(SyncError::Protocol("version user agent or length"));
    }
    // Modern peers may omit relay. Supply its protocol default without permitting
    // rust-bitcoin's string decoder to see an unchecked attacker-provided count.
    let mut bytes = payload.to_vec();
    if tail.len() == prefix + len + 4 {
        bytes.push(1);
    }
    deserialize(&bytes).map_err(|_| SyncError::Protocol("version encoding"))
}
