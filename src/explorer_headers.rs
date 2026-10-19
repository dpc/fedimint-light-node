//! Instance-local reverse cursors; untrusted evidence, never validation checkpoints.

use std::time::Duration;

use bitcoin::{BlockHash, block::Header};
use tokio::time::Instant;

use crate::explorer_header_config::ExplorerHeaderConfig;
use crate::explorer_http::ExplorerHttp;
use crate::sync_error::SyncError;

#[cfg(test)]
#[path = "explorer_headers_tests.rs"]
mod tests;

/// Global application-request pacing and fair independently jittered source visits.
pub(crate) struct ExplorerHeaders {
    /// One shared hardened transport; no per-source task.
    pub http: ExplorerHttp,
    /// Bounded independent reverse cursors, separate from the P2P candidate.
    pub sources: Vec<Source>,
    /// Next source examined first, rotated before every await.
    next: usize,
    /// Earliest global request start, including canceled attempts.
    paced: Instant,
    /// Minimum of 256, configured candidate bound and store capacity.
    pub cap: usize,
}

/// Scheduling and immutable-by-hash evidence for one configured transport.
pub(crate) struct Source {
    /// Next regular visit, reserved before its first request.
    pub due: Instant,
    /// Active visit expiry; no request may start after it.
    until: Instant,
    /// Remaining attempts, charged before awaiting.
    remaining: usize,
    /// Consecutive failed visits, capped to bound exponential backoff.
    failures: u32,
    /// Pinned snapshot's next missing predecessor, not a freshness claim.
    pub expected: Option<BlockHash>,
    /// Snapshot tip first; every adjacent pair links by locally computed hash.
    pub reverse: Vec<Header>,
    /// Last selection generation checked for newly intersecting cached ancestry.
    pub generation: u64,
    /// Excessive valid Retry-After suspends rather than retrying before permission.
    pub suspended: bool,
}

impl ExplorerHeaders {
    pub fn new(config: ExplorerHeaderConfig, cap: usize) -> Result<Self, SyncError> {
        let http = ExplorerHttp::new(&config)?;
        let now = Instant::now();
        let sources = config
            .roots
            .iter()
            .map(|_| Source {
                due: now + Duration::from_secs(rand::random_range(0..=1800)),
                until: now,
                remaining: 0,
                failures: 0,
                expected: None,
                reverse: Vec::new(),
                generation: 0,
                suspended: false,
            })
            .collect();
        Ok(Self {
            http,
            sources,
            next: 0,
            paced: now,
            cap: cap.min(256),
        })
    }

    /// Earliest opportunity; the owner sleeps outside its exclusive work operation.
    pub fn deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        self.sources
            .iter()
            .filter(|source| !source.suspended)
            .map(|source| if source.active(now) { now } else { source.due })
            .min()
            .map(|due| due.max(self.paced))
    }

    /// Reserve all cancellation-sensitive accounting before returning a source.
    pub fn reserve(&mut self) -> Option<usize> {
        let now = Instant::now();
        if now < self.paced {
            return None;
        }
        for offset in 0..self.sources.len() {
            let index = (self.next + offset) % self.sources.len();
            let source = &mut self.sources[index];
            if source.suspended {
                continue;
            }
            if !source.active(now) {
                if now < source.due {
                    continue;
                }
                source.due = now + Duration::from_secs(rand::random_range(900..=2700));
                source.until = now + Duration::from_secs(120);
                source.remaining = 16;
            }
            source.remaining -= 1;
            self.next = (index + 1) % self.sources.len();
            self.paced = now + Duration::from_secs(1);
            return Some(index);
        }
        None
    }
}

impl Source {
    /// Never start an attempt that cannot fit its full nominal HTTP deadline.
    fn active(&self, now: Instant) -> bool {
        self.remaining > 0 && now + crate::explorer_http::REQUEST_TIMEOUT <= self.until
    }

    /// End a completed snapshot without retaining a validity or freshness verdict.
    pub fn complete(&mut self) {
        self.expected = None;
        self.reverse.clear();
        self.remaining = 0;
        self.failures = 0;
    }

    /// Back off this source only. Transport failures retain the pinned snapshot.
    pub fn fail(&mut self, retry: Duration, discard: bool) {
        if discard {
            self.expected = None;
            self.reverse.clear();
        }
        self.remaining = 0;
        self.failures = (self.failures + 1).min(6);
        let ceiling = (3600_u64 << (self.failures - 1)).min(86400);
        let backoff = Duration::from_secs(rand::random_range(ceiling * 3 / 4..=ceiling));
        self.due = self.due.max(Instant::now() + backoff.max(retry));
    }
}
