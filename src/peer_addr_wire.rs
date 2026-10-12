//! Streaming legacy address admission: no peer-sized payload/vector allocation.

use std::time::{SystemTime, UNIX_EPOCH};

use bitcoin::hashes::{Hash, HashEngine, sha256d};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

use crate::header_wire::Message;
use crate::peer_address::{discoverable, legacy};
use crate::sync_error::SyncError;

/// Check canonical count and exact frame length before creating even the bounded sample.
pub(crate) async fn receive_addresses(
    stream: &mut TcpStream,
    len: usize,
    checksum: &[u8],
) -> Result<Message, SyncError> {
    if len == 0 {
        return Err(SyncError::Protocol("empty addr"));
    }
    let mut engine = sha256d::Hash::engine();
    let first = stream.read_u8().await?;
    engine.input(&[first]);
    let (count, prefix) = match first {
        0..=252 => (usize::from(first), 1),
        253 if len >= 3 => {
            let mut bytes = [0; 2];
            stream.read_exact(&mut bytes).await?;
            engine.input(&bytes);
            let n = usize::from(u16::from_le_bytes(bytes));
            if n < 253 {
                return Err(SyncError::Protocol("noncanonical addr count"));
            }
            (n, 3)
        }
        _ => return Err(SyncError::Protocol("excessive addr count")),
    };
    if count > 1000 || len != prefix + count * 30 {
        return Err(SyncError::Protocol("addr count or length"));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut addresses = Vec::with_capacity(count.min(32));
    let mut eligible = 0usize;
    for _ in 0..count {
        let mut entry = [0; 30];
        stream.read_exact(&mut entry).await?;
        engine.input(&entry);
        let time = u64::from(u32::from_le_bytes(entry[..4].try_into().expect("fixed")));
        let services = u64::from_le_bytes(entry[4..12].try_into().expect("fixed"));
        let addr = legacy(
            entry[12..28].try_into().expect("fixed"),
            u16::from_be_bytes(entry[28..].try_into().expect("fixed")),
        );
        if services & 9 != 9
            || time < now.saturating_sub(14 * 86400)
            || time > now.saturating_add(600)
            || !discoverable(addr)
        {
            continue;
        }
        eligible += 1;
        if addresses.len() < 32 {
            addresses.push(addr);
        } else {
            let index = rand::random_range(0..eligible);
            if index < 32 {
                addresses[index] = addr;
            }
        }
    }
    if &sha256d::Hash::from_engine(engine)[..4] != checksum {
        return Err(SyncError::Protocol("addr checksum"));
    }
    Ok(Message::Addresses(addresses))
}
