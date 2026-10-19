//! Bounded checksummed legacy inventory admission shared by idle and leased readers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bitcoin::Txid;
use bitcoin::hashes::{Hash, HashEngine, sha256d};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::Instant;

use crate::sync_error::SyncError;

/// Fixed aggregate owner inbox capacity.
const INBOX: usize = 4096;
/// Each of at most four sockets stages at most this many IDs until checksum commit.
const STAGING: usize = 1024;

/// Aggregate inventory state; no cache or chain access from socket tasks.
pub(crate) struct InventoryInbox {
    /// Accepted fixed-size observations awaiting the exclusive owner.
    pub queue: VecDeque<(Txid, u64)>,
    /// Complete-frame observations discarded by staging/inbox pressure.
    pub dropped: u64,
    /// Fixed-minute inventory window, independent of control and body traffic.
    window: Instant,
    /// Aggregate declared wire bytes, charged even on failure or cancellation.
    bytes: usize,
}

impl InventoryInbox {
    /// Allocate no transaction state until explicitly enabled.
    pub fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            queue: VecDeque::new(),
            dropped: 0,
            window: Instant::now(),
            bytes: 8 * 1024 * 1024,
        }))
    }
}

/// Per-connection accounting survives idle/foreground lease transitions.
pub(crate) struct InventoryObserver {
    /// Shared aggregate inbox and declared-byte allowance.
    inbox: Arc<Mutex<InventoryInbox>>,
    /// Stable prefix identity; reconnects do not create new source groups.
    source: u64,
    /// Fixed-minute per-connection window.
    window: Instant,
    /// Declared-byte allowance, including incomplete frames.
    bytes: usize,
    /// Independent inventory-frame allowance, not the control frame count.
    frames: usize,
}

impl InventoryObserver {
    /// Construct only after an explicitly relay-enabled handshake.
    pub fn new(inbox: Arc<Mutex<InventoryInbox>>, source: u64) -> Self {
        Self {
            inbox,
            source,
            window: Instant::now(),
            bytes: 2 * 1024 * 1024,
            frames: 1024,
        }
    }

    /// Reserve a complete declared frame before reading any payload; never refund.
    fn reserve(&mut self, length: usize) -> Result<(), SyncError> {
        if self.window.elapsed() >= Duration::from_secs(60) {
            self.window = Instant::now();
            self.bytes = 2 * 1024 * 1024;
            self.frames = 1024;
        }
        let mut inbox = self.inbox.lock().expect("no panicking inbox owner");
        if inbox.window.elapsed() >= Duration::from_secs(60) {
            inbox.window = Instant::now();
            inbox.bytes = 8 * 1024 * 1024;
        }
        if self.frames == 0 || length > self.bytes || length > inbox.bytes {
            return Err(SyncError::Protocol("inventory rate budget"));
        }
        self.frames -= 1;
        self.bytes -= length;
        inbox.bytes -= length;
        Ok(())
    }

    /// Stream exact canonical inventory shape and commit only after checksum success.
    pub async fn receive(
        &mut self,
        stream: &mut TcpStream,
        len: usize,
        checksum: &[u8],
    ) -> Result<(), SyncError> {
        self.reserve(24 + len)?;
        if len == 0 || len > 1_800_003 {
            return Err(SyncError::Protocol("inventory length"));
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
                let count = usize::from(u16::from_le_bytes(bytes));
                if count < 253 {
                    return Err(SyncError::Protocol("noncanonical inventory count"));
                }
                (count, 3)
            }
            _ => return Err(SyncError::Protocol("inventory count")),
        };
        if count > 50_000 || len != prefix + 36 * count {
            return Err(SyncError::Protocol("inventory shape"));
        }
        let mut staged = Vec::new();
        let mut dropped = 0_u64;
        for _ in 0..count {
            let mut item = [0; 36];
            stream.read_exact(&mut item).await?;
            engine.input(&item);
            // MSG_WTX and witness/block/unknown inventory are NOT legacy txids.
            if u32::from_le_bytes(item[..4].try_into().expect("fixed type")) == 1 {
                if staged.len() < STAGING {
                    staged.push(Txid::from_byte_array(
                        item[4..].try_into().expect("fixed txid"),
                    ));
                } else {
                    dropped += 1;
                }
            }
        }
        if &sha256d::Hash::from_engine(engine)[..4] != checksum {
            return Err(SyncError::Protocol("inventory checksum"));
        }
        let mut inbox = self.inbox.lock().expect("no panicking inbox owner");
        for txid in staged {
            if inbox.queue.len() < INBOX {
                inbox.queue.push_back((txid, self.source));
            } else {
                dropped += 1;
            }
        }
        inbox.dropped = inbox.dropped.saturating_add(dropped);
        Ok(())
    }
}

#[cfg(test)]
#[path = "inventory_observer_tests.rs"]
mod tests;
