use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::RngExt;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};

use crate::idle_peer::{IdleBudget, IdlePeer};
use crate::peer_address::group;
use crate::peer_book::PeerBook;
use crate::peer_connection::PeerConnection;
use crate::peer_handshake::handshake_inner;
use crate::peer_pool_config::PeerPoolConfig;
use crate::sync_error::SyncError;

/// Metadata survives moving the stream between exactly one reader and foreground owner.
struct Slot {
    /// Numeric dial target.
    addr: SocketAddr,
    /// Stable slots retain checked useful service; the last slot explores.
    stable: bool,
    /// Provisional/exploratory rotation deadline, deferred while leased.
    expires: Instant,
    /// Sole idle socket owner.
    idle: IdlePeer,
}

/// Foreground ownership and the failure reservation made for this exact attempt.
struct Lease {
    /// Numeric dial target, which cannot be replaced while exclusively leased.
    addr: SocketAddr,
    /// Stable/explorer role before checkout.
    stable: bool,
    /// Monotonic connection retirement deadline.
    expires: Instant,
    /// Failure count before this attempt, including saturation at the counter limit.
    previous_failures: Option<u8>,
}

/// Bounded caller-driven dial policy with actively maintained idle connections.
///
/// No runtime is created. At most four sockets and four idle tasks exist; a dial
/// occupies a vacant slot, so no hidden replacement sockets exceed that cap.
/// Operations lease exactly one stream. Cancellation closes that stream rather
/// than returning a partially read frame. Drop aborts drivers; `shutdown` joins.
pub(crate) struct PeerPool {
    /// Explicit finite policy.
    config: PeerPoolConfig,
    /// Bounded endpoint availability state.
    book: PeerBook,
    /// Established idle slots.
    slots: Vec<Slot>,
    /// One foreground slot reserved before await, including canceled acquisition.
    leased: Option<Lease>,
    /// Keep canceled checkout/retirement handles until joined before any new dial.
    retiring: Vec<IdlePeer>,
    /// Clean-boundary transfer in progress; cancellation must not detach its handle.
    checking_out: Option<IdlePeer>,
    /// Dial pacing reservation, survives cancellation.
    next_dial: Instant,
    /// A failed/canceled exploratory dial gives maintained sockets the next turn.
    failed_exploration: bool,
    /// One shared control-traffic allowance.
    idle_budget: Arc<Mutex<IdleBudget>>,
    /// One retained DNS job; a timeout never starts a replacement while unresolved.
    resolver: Option<JoinHandle<std::io::Result<Vec<SocketAddr>>>>,
    /// Seed index associated with the current DNS job.
    seed: usize,
    /// Earliest next seed cycle, at most once every thirty minutes.
    next_seed_cycle: Instant,
    /// Cache failures are observable without becoming chain failures.
    cache_failed: bool,
    /// Explicit observation opt-in, installed before dialing any sockets.
    pub inbox: Option<Arc<Mutex<crate::inventory_observer::InventoryInbox>>>,
}

impl Drop for PeerPool {
    fn drop(&mut self) {
        if let Some(job) = &self.resolver {
            job.abort();
        }
    }
}

impl PeerPool {
    /// Validate caller policy and load only bounded disposable availability history.
    pub fn new(manual: &[SocketAddr], config: PeerPoolConfig) -> Result<Self, SyncError> {
        if manual.len() > 16 || !config.valid() || (!config.discovery && manual.is_empty()) {
            return Err(SyncError::Configuration);
        }
        let mut book = PeerBook::new(manual);
        #[cfg(test)]
        if !config.discovered_for_test.is_empty() {
            // Loopback fixture transport replaces already-admitted public targets;
            // production DNS/gossip/cache admission never has this bypass.
            assert!(config.discovered_for_test.len() <= 16);
            for mut entry in PeerBook::new(&config.discovered_for_test).entries {
                entry.manual = false;
                book.entries.push(entry);
            }
        }
        if config.discovery
            && let Some(path) = &config.cache
        {
            crate::peer_cache::load(path, &mut book);
        }
        Ok(Self {
            config,
            book,
            slots: Vec::new(),
            leased: None,
            retiring: Vec::new(),
            checking_out: None,
            next_dial: Instant::now(),
            failed_exploration: false,
            idle_budget: Arc::new(Mutex::new(IdleBudget::new())),
            resolver: None,
            seed: 0,
            next_seed_cycle: Instant::now(),
            cache_failed: false,
            inbox: None,
        })
    }

    /// Return connected/candidate counts; neither establishes freshness or honesty.
    pub fn counts(&self) -> (usize, usize, bool) {
        (
            self.slots.iter().filter(|s| !s.idle.finished()).count(),
            self.book.entries.len(),
            self.cache_failed,
        )
    }

    /// Manual mode tries each configured candidate at most once per bounded operation.
    pub fn attempt_limit(&self) -> usize {
        if self.config.discovery {
            16
        } else {
            self.book.entries.len()
        }
    }

    /// Identify an attempt reserved before a canceled handshake or pacing wait.
    pub fn leased_address(&self) -> Option<SocketAddr> {
        self.leased.as_ref().map(|lease| lease.addr)
    }

    /// Harvest coalesced hints and retire failed, old or exploratory sockets.
    async fn maintain(&mut self) {
        // An outstanding marker here means the preceding operation was canceled.
        self.leased = None;
        if let Some(idle) = self.checking_out.take() {
            self.retiring.push(idle);
        }
        let now = Instant::now();
        let mut index = 0;
        while index < self.slots.len() {
            let slot = &mut self.slots[index];
            if self.config.discovery && slot.idle.hints.has_changed().unwrap_or(false) {
                for addr in slot.idle.hints.borrow_and_update().iter() {
                    self.book.admit(*addr, group(slot.addr));
                }
            }
            if slot.idle.finished() || slot.expires <= now {
                let slot = self.slots.remove(index);
                self.book.attempted(slot.addr, Duration::from_secs(30));
                self.retiring.push(slot.idle);
            } else {
                index += 1;
            }
        }
        while let Some(idle) = self.retiring.last_mut() {
            idle.shutdown().await;
            self.retiring.pop();
        }
        let connected: Vec<_> = self.slots.iter().map(|s| s.addr).collect();
        self.book.age(&connected);
    }

    async fn discover(&mut self, wait: Duration) {
        if !self.config.discovery || self.config.dns_seeds.is_empty() {
            return;
        }
        if self.resolver.is_none() && Instant::now() >= self.next_seed_cycle {
            let host = self.config.dns_seeds[self.seed].clone();
            self.resolver = Some(tokio::spawn(async move {
                // lookup_host's OS resolver may allocate more results internally;
                // only 32 numeric results enter application state.
                Ok(tokio::net::lookup_host((host.as_str(), 8333))
                    .await?
                    .take(32)
                    .collect())
            }));
        }
        let Some(job) = &mut self.resolver else {
            return;
        };
        let Ok(result) = timeout(wait, job).await else {
            return;
        };
        self.resolver = None;
        if let Ok(Ok(addresses)) = result {
            for addr in addresses {
                self.book.admit(addr, (1 << 63) | self.seed as u64);
            }
        }
        self.seed += 1;
        if self.seed == self.config.dns_seeds.len() {
            self.seed = 0;
            self.next_seed_cycle = Instant::now() + Duration::from_secs(1800);
        }
    }

    /// Acquire one clean connection; byte reservations include any new handshake.
    #[allow(clippy::result_large_err)] // One bounded foreground error, never queued or replicated.
    pub async fn acquire(
        &mut self,
        height: u64,
        pacing: Duration,
        deadline: Duration,
        received: &mut usize,
        sent: &mut usize,
        explore: bool,
    ) -> Result<PeerConnection, (Option<SocketAddr>, SyncError)> {
        self.maintain().await;
        self.discover(if self.book.entries.is_empty() {
            deadline
        } else {
            Duration::ZERO
        })
        .await;
        let connected: Vec<_> = self.slots.iter().map(|s| s.addr).collect();
        let prefer_existing = std::mem::take(&mut self.failed_exploration);
        if self.slots.len() < self.config.connections
            && (self.slots.is_empty() || (explore && !prefer_existing))
            && let Some(addr) = self.book.choose(
                &connected,
                self.slots.iter().filter(|s| s.stable).count() >= self.config.connections - 1,
            )
        {
            let stable =
                self.slots.iter().filter(|s| s.stable).count() < self.config.connections - 1;
            let rotation = self
                .config
                .rotation
                .mul_f64(self.book.rng.random_range(1.0..2.0));
            self.leased = Some(Lease {
                addr,
                stable,
                expires: Instant::now() + rotation,
                previous_failures: self.book.attempted(addr, pacing),
            });
            self.failed_exploration = true;
            let when = self.next_dial.max(Instant::now()) + pacing;
            self.next_dial = when;
            sleep_until(when).await;
            let result = timeout(
                deadline,
                handshake_inner(addr, height, received, sent, self.inbox.is_some()),
            )
            .await;
            let (stream, version) = result
                .map_err(|_| (Some(addr), SyncError::Timeout))?
                .map_err(|e| (Some(addr), e))?;
            return Ok(PeerConnection {
                stream,
                addr,
                services: version.services,
                relay: version.version >= 70012 && version.relay,
                learned: Vec::new(),
                requested_addresses: false,
                idle_window: Instant::now(),
                idle_bytes: 256 * 1024,
                idle_frames: 64,
                observer: self.inbox.as_ref().map(|inbox| {
                    crate::inventory_observer::InventoryObserver::new(inbox.clone(), group(addr))
                }),
            });
        }
        if self.slots.is_empty() {
            // Empty/unavailable discovery cannot create a tight retry loop.
            sleep_until(Instant::now() + pacing).await;
            return Err((None, SyncError::NoPeers));
        }
        // Release appends to the back, giving every live connection one turn.
        let slot = self.slots.remove(0);
        self.leased = Some(Lease {
            addr: slot.addr,
            stable: slot.stable,
            expires: slot.expires,
            previous_failures: self.book.attempted(slot.addr, pacing),
        });
        self.checking_out = Some(slot.idle);
        let result = timeout(
            deadline,
            self.checking_out.as_mut().expect("owned checkout").take(),
        )
        .await
        .map_err(|_| (Some(slot.addr), SyncError::Timeout))?
        .map_err(|e| (Some(slot.addr), e));
        // Awaiting the JoinHandle completed, so the returned stream or error is
        // consumed. Remove the handle before any later cancellation point.
        self.checking_out = None;
        result
    }

    /// Discard a time-rejected lease without attributing local clock uncertainty to a peer.
    ///
    /// Only this attempt's provisional strike is undone. Its monotonic retry
    /// reservation and failed-exploration fallback remain, with no useful credit.
    /// The exclusive foreground owner calls this before dropping the connection;
    /// no book aging/admission or replacement occurs during a lease.
    pub fn rejected_time(&mut self, peer: &PeerConnection) {
        if !self
            .leased
            .as_ref()
            .is_some_and(|lease| lease.addr == peer.addr)
        {
            return;
        }
        let lease = self.leased.take().expect("matched lease");
        if let Some(previous) = lease.previous_failures
            && let Some(entry) = self.book.entries.iter_mut().find(|e| e.addr == lease.addr)
        {
            entry.failures = previous;
        }
    }

    /// Return only a fully completed exchange; useful means checked headers or body.
    pub fn release(&mut self, mut peer: PeerConnection, useful: bool) {
        let Some(Lease {
            addr,
            mut stable,
            mut expires,
            ..
        }) = self.leased.take()
        else {
            return;
        };
        if addr != peer.addr {
            return;
        }
        // A handshake alone is not successful foreground service. Keep the
        // fallback reservation until the entire exchange completed cleanly.
        self.failed_exploration = false;
        self.book.completed(addr, useful);
        if self.config.discovery {
            for hint in peer.learned.drain(..) {
                self.book.admit(hint, group(addr));
            }
        }
        if useful {
            stable |= self.slots.iter().filter(|s| s.stable).count() < self.config.connections - 1;
            if stable {
                expires = Instant::now() + Duration::from_secs(86400);
            }
            if self.config.discovery
                && let Some(path) = &self.config.cache
            {
                self.cache_failed = crate::peer_cache::save(path, &self.book).is_err();
            }
        }
        self.slots.push(Slot {
            addr,
            stable,
            expires,
            idle: IdlePeer::start(peer, self.config.discovery, Arc::clone(&self.idle_budget)),
        });
    }

    /// Close all sockets and join idle workers; OS resolver calls may outlive cancellation.
    pub async fn shutdown(&mut self) {
        self.leased = None;
        if let Some(idle) = self.checking_out.take() {
            self.retiring.push(idle);
        }
        self.retiring.extend(self.slots.drain(..).map(|s| s.idle));
        while let Some(idle) = self.retiring.last_mut() {
            idle.shutdown().await;
            self.retiring.pop();
        }
        if let Some(mut job) = self.resolver.take() {
            job.abort();
            let _ = (&mut job).await;
        }
    }
}

#[cfg(test)]
#[path = "peer_pool_tests.rs"]
mod tests;
