use std::sync::{Arc, Mutex};
use std::time::Duration;

use bitcoin::p2p::message::NetworkMessage;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};

use crate::header_wire::{Message, send};
use crate::peer_connection::PeerConnection;
use crate::sync_error::SyncError;

/// Shared aggregate idle receive allowance, independent of the foreground body budget.
pub(crate) struct IdleBudget {
    /// Fixed one-minute accounting window.
    start: Instant,
    /// Remaining aggregate declared bytes, conservatively reserved before reading.
    bytes: usize,
}

/// A reservation belongs to exactly one fixed accounting window.
pub(crate) struct Reservation {
    /// Maximum declared frame bytes held until completion.
    pub bytes: usize,
    /// Window identity; refunds from old windows cannot replenish the new one.
    epoch: Instant,
}

impl IdleBudget {
    /// Start a full aggregate one-minute allowance.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            bytes: 1024 * 1024,
        }
    }

    /// Reserve before receiving so concurrent idle frames share one bounded allowance.
    pub(crate) fn reserve(&mut self, limit: usize) -> Reservation {
        if self.start.elapsed() >= Duration::from_secs(60) {
            self.start = Instant::now();
            self.bytes = 1024 * 1024;
        }
        let n = self.bytes.min(limit);
        self.bytes -= n;
        Reservation {
            bytes: n,
            epoch: self.start,
        }
    }

    /// Refund only to the window that issued the reservation.
    pub(crate) fn refund(&mut self, reservation: Reservation, remaining: usize) {
        if self.start == reservation.epoch {
            self.bytes += remaining.min(reservation.bytes);
        }
    }
}

/// Owns and cancels a single idle reader, including any completed unconsumed output.
pub(crate) struct IdlePeer {
    /// One lease command, with no unbounded channel.
    request: Option<oneshot::Sender<()>>,
    /// Single owned task; Drop aborts, shutdown additionally joins.
    task: JoinHandle<()>,
    /// The lease result receiver moves into the checkout future, so canceling
    /// even a completed-but-unconsumed transfer immediately drops its stream.
    result: Option<oneshot::Receiver<Result<PeerConnection, SyncError>>>,
    /// Coalesced hints: at most one 32-address sample.
    pub hints: watch::Receiver<Vec<std::net::SocketAddr>>,
}

/// Cancel the idle owner if a clean-boundary transfer future is dropped.
struct AbortTransfer(tokio::task::AbortHandle);

impl Drop for AbortTransfer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Drop for IdlePeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl IdlePeer {
    /// Start control-frame maintenance on the caller's already-running Tokio runtime.
    pub fn start(peer: PeerConnection, discovery: bool, budget: Arc<Mutex<IdleBudget>>) -> Self {
        let (request, command) = oneshot::channel();
        let (hints, receive) = watch::channel(Vec::new());
        let (output, result) = oneshot::channel();
        Self {
            request: Some(request),
            task: tokio::spawn(async move {
                let result = run(peer, discovery, budget, command, hints).await;
                let _ = output.send(result);
            }),
            result: Some(result),
            hints: receive,
        }
    }

    /// Lease only after the current frame completes; cancellation aborts the owner.
    pub async fn take(&mut self) -> Result<PeerConnection, SyncError> {
        let _abort = AbortTransfer(self.task.abort_handle());
        let result = self
            .result
            .take()
            .ok_or(SyncError::Protocol("duplicate checkout"))?;
        if let Some(request) = self.request.take() {
            let _ = request.send(());
        }
        let result = result
            .await
            .map_err(|_| SyncError::Protocol("idle task stopped"))?;
        (&mut self.task)
            .await
            .map_err(|_| SyncError::Protocol("idle task stopped"))?;
        result
    }

    /// Report dead control drivers so the next maintenance pass can refill.
    pub fn finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Cancel and join the owned driver, dropping any stream in its result.
    pub async fn shutdown(&mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

async fn run(
    mut peer: PeerConnection,
    discovery: bool,
    budget: Arc<Mutex<IdleBudget>>,
    mut command: oneshot::Receiver<()>,
    hints: watch::Sender<Vec<std::net::SocketAddr>>,
) -> Result<PeerConnection, SyncError> {
    if discovery && !peer.requested_addresses {
        timeout(
            Duration::from_secs(10),
            send(&mut peer.stream, NetworkMessage::GetAddr),
        )
        .await
        .map_err(|_| SyncError::Timeout)??;
        peer.requested_addresses = true;
    }
    let mut next_ping = Instant::now() + Duration::from_secs(120);
    let mut pending_ping = None;
    loop {
        // Only peek races a lease command. Once a frame starts we finish exactly
        // that frame or close on its absolute deadline: read_exact is NOT canceled
        // and resumed on a reusable stream.
        let mut peek = [0];
        tokio::select! {
            biased;
            _ = &mut command => return Ok(peer),
            _ = sleep_until(next_ping) => {
                if pending_ping.is_some() { return Err(SyncError::Timeout); }
                let nonce = rand::random();
                timeout(Duration::from_secs(10), send(&mut peer.stream, NetworkMessage::Ping(nonce)))
                    .await.map_err(|_| SyncError::Timeout)??;
                pending_ping = Some(nonce);
                next_ping = Instant::now() + Duration::from_secs(30);
                continue;
            }
            result = peer.stream.peek(&mut peek) => {
                if result? == 0 { return Err(SyncError::Protocol("idle disconnect")); }
            }
        }
        if peer.idle_window.elapsed() >= Duration::from_secs(60) {
            peer.idle_window = Instant::now();
            peer.idle_bytes = 256 * 1024;
            peer.idle_frames = 64;
        }
        if peer.idle_frames == 0 {
            return Err(SyncError::Protocol("idle frame rate"));
        }
        peer.idle_frames -= 1;
        let reserved = budget
            .lock()
            .expect("no panicking budget owner")
            .reserve(peer.idle_bytes);
        let mut remaining = reserved.bytes;
        let message = timeout(Duration::from_secs(10), peer.receive(&mut remaining, None))
            .await
            .map_err(|_| SyncError::Timeout)??;
        peer.idle_bytes -= reserved.bytes - remaining;
        {
            let mut shared = budget.lock().expect("no panicking budget owner");
            shared.refund(reserved, remaining);
        }
        match message {
            Message::Ping(nonce) => {
                timeout(
                    Duration::from_secs(10),
                    send(&mut peer.stream, NetworkMessage::Pong(nonce)),
                )
                .await
                .map_err(|_| SyncError::Timeout)??;
            }
            Message::Pong(nonce) if pending_ping == Some(nonce) => {
                pending_ping = None;
                next_ping = Instant::now() + Duration::from_secs(120);
            }
            // Unsolicited headers are bounded hints, not validated progress.
            Message::Ignore | Message::Pong(_) | Message::Headers(_) => {}
            _ => return Err(SyncError::Protocol("unexpected idle message")),
        }
        if discovery && !peer.learned.is_empty() {
            hints.send_replace(std::mem::take(&mut peer.learned));
        } else {
            peer.learned.clear();
        }
    }
}
