//! Test-only observations of natural transport occupancy; never substitutes results.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

thread_local! {
    static CURRENT: RefCell<Option<Arc<State>>> = const { RefCell::new(None) };
}

/// Live resources whose cancellation must release the observation too.
#[derive(Clone, Copy)]
pub(crate) enum Counter {
    Accepted,
    Writer,
    BlockedWriter,
    Peer,
}

/// One isolated current-thread test's observations.
#[derive(Default)]
pub(crate) struct State {
    /// Accepted HTTP tasks.
    pub accepted: AtomicUsize,
    /// HTTP tasks serializing their response to a socket.
    pub writers: AtomicUsize,
    /// Actual body write futures that returned Pending.
    pub blocked: AtomicUsize,
    /// Successfully enqueued, not yet dequeued cheap work.
    pub cheap: AtomicUsize,
    /// Successfully enqueued, not yet dequeued network work.
    pub network: AtomicUsize,
    /// Fixture-side live P2P sockets, including the foreground lease.
    pub peers: AtomicUsize,
    /// Small bounded samples; no body bytes retained here.
    pub samples: Mutex<Vec<Snapshot>>,
}

/// Occupancy and process measurements at one natural execution point.
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    /// Natural point or phase identifier.
    pub label: &'static str,
    /// Concurrent accepted HTTP tasks.
    pub accepted: usize,
    /// HTTP response write futures.
    pub writers: usize,
    /// Write futures that actually returned Pending.
    pub blocked: usize,
    /// Queued cheap requests.
    pub cheap: usize,
    /// Queued network requests.
    pub network: usize,
    /// Open fixture-side P2P sockets, including leased sockets.
    pub peers: usize,
    /// Current process resident memory including harness allocations.
    pub rss_kib: usize,
    /// Process lifetime resident memory high-water mark.
    pub hwm_kib: usize,
    /// Process open descriptors, including the observation directory.
    pub fds: usize,
}

impl State {
    fn counter(&self, kind: Counter) -> &AtomicUsize {
        match kind {
            Counter::Accepted => &self.accepted,
            Counter::Writer => &self.writers,
            Counter::BlockedWriter => &self.blocked,
            Counter::Peer => &self.peers,
        }
    }

    pub fn snapshot(&self, label: &'static str) -> Snapshot {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let memory = |name: &str| {
            status
                .lines()
                .find_map(|line| {
                    line.strip_prefix(name)?
                        .split_whitespace()
                        .next()?
                        .parse()
                        .ok()
                })
                .unwrap()
        };
        Snapshot {
            label,
            accepted: self.accepted.load(Ordering::SeqCst),
            writers: self.writers.load(Ordering::SeqCst),
            blocked: self.blocked.load(Ordering::SeqCst),
            cheap: self.cheap.load(Ordering::SeqCst),
            network: self.network.load(Ordering::SeqCst),
            peers: self.peers.load(Ordering::SeqCst),
            rss_kib: memory("VmRSS:"),
            hwm_kib: memory("VmHWM:"),
            fds: std::fs::read_dir("/proc/self/fd").unwrap().count(),
        }
    }
}

/// Decrement on normal completion, error, or cancellation.
pub(crate) struct Guard {
    /// Owning test's independent counters.
    state: Arc<State>,
    /// Resource released by this guard.
    kind: Counter,
}

impl Drop for Guard {
    fn drop(&mut self) {
        assert!(self.state.counter(self.kind).fetch_sub(1, Ordering::SeqCst) > 0);
    }
}

/// Install only on the ignored test's current-thread runtime.
pub(crate) fn install(state: Arc<State>) {
    CURRENT.with(|current| {
        assert!(current.borrow().is_none());
        *current.borrow_mut() = Some(state);
    });
}

pub(crate) fn clear() {
    CURRENT.with(|current| *current.borrow_mut() = None);
}

pub(crate) fn enter(kind: Counter) -> Option<Guard> {
    CURRENT.with(|current| {
        current.borrow().as_ref().map(|state| {
            state.counter(kind).fetch_add(1, Ordering::SeqCst);
            Guard {
                state: state.clone(),
                kind,
            }
        })
    })
}

pub(crate) fn queue(network: bool, enqueued: bool) {
    CURRENT.with(|current| {
        if let Some(state) = current.borrow().as_ref() {
            let count = if network {
                &state.network
            } else {
                &state.cheap
            };
            if enqueued {
                count.fetch_add(1, Ordering::SeqCst);
            } else {
                assert!(count.fetch_sub(1, Ordering::SeqCst) > 0);
            }
        }
    });
}

pub(crate) fn observe(label: &'static str) {
    CURRENT.with(|current| {
        if let Some(state) = current.borrow().as_ref() {
            let snapshot = state.snapshot(label);
            let mut samples = state.samples.lock().unwrap();
            assert!(samples.len() < 64, "bounded observation log exhausted");
            samples.push(snapshot);
        }
    });
}
