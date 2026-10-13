use std::net::SocketAddr;
use std::time::Duration;

use rand::RngExt;
use rand::rngs::StdRng;
use tokio::time::Instant;

use crate::peer_address::{discoverable, group, normalize};

/// One bounded availability hint; remote advertisements cannot reset local observations.
pub(crate) struct Candidate {
    /// Numeric normalized endpoint.
    pub addr: SocketAddr,
    /// Original admitting source group, not overwritten by repeated gossip.
    pub source: u64,
    /// Explicit local/private configuration bypasses only public-address/diversity policy.
    pub manual: bool,
    /// Time of last checked useful service.
    pub useful: Option<Instant>,
    /// Local admission time; remote timestamps cannot extend retention.
    pub added: Instant,
    /// Next permitted attempt, reserved before awaiting.
    pub retry: Instant,
    /// Consecutive unsuccessful attempts.
    pub failures: u8,
}

/// Separate finite new/successful populations with source-uniform selection.
pub(crate) struct PeerBook {
    /// At most sixteen manual, 256 new and 64 successful records.
    pub entries: Vec<Candidate>,
    /// Independently OS-seeded per service, injectable in unit tests.
    pub rng: StdRng,
}

impl PeerBook {
    /// Deduplicate explicitly configured peers and seed an independent random selector.
    pub fn new(manual: &[SocketAddr]) -> Self {
        let now = Instant::now();
        let mut book = Self {
            entries: Vec::new(),
            rng: rand::make_rng(),
        };
        for &addr in manual {
            let addr = normalize(addr);
            if !book.entries.iter().any(|e| e.addr == addr) {
                book.entries.push(Candidate {
                    addr,
                    source: group(addr),
                    manual: true,
                    useful: None,
                    added: now,
                    retry: now,
                    failures: 0,
                });
            }
        }
        book
    }

    /// Admit a public hint only within new-population source and destination quotas.
    pub fn admit(&mut self, addr: SocketAddr, source: u64) {
        let addr = normalize(addr);
        if !discoverable(addr) || self.entries.iter().any(|e| e.addr == addr) {
            return;
        }
        let new = self
            .entries
            .iter()
            .filter(|e| !e.manual && e.useful.is_none());
        if new.clone().count() >= 256
            || new.clone().filter(|e| e.source == source).count() >= 32
            || new.filter(|e| group(e.addr) == group(addr)).count() >= 4
        {
            return;
        }
        let now = Instant::now();
        self.entries.push(Candidate {
            addr,
            source,
            manual: false,
            useful: None,
            added: now,
            retry: now,
            failures: 0,
        });
    }

    /// Expire locally aged hints while preserving explicit and connected records.
    pub fn age(&mut self, connected: &[SocketAddr]) {
        let now = Instant::now();
        self.entries.retain(|e| {
            e.manual
                || connected.contains(&e.addr)
                || e.useful
                    .is_some_and(|t| now.duration_since(t) < Duration::from_secs(7 * 86400))
                || (e.useful.is_none() && now.duration_since(e.added) < Duration::from_secs(86400))
        });
    }

    /// Select an eligible source uniformly, then an address without relaxing diversity.
    pub fn choose(&mut self, connected: &[SocketAddr], explorer: bool) -> Option<SocketAddr> {
        let now = Instant::now();
        let eligible = |e: &&Candidate| {
            e.retry <= now
                && (e.manual || e.useful.is_some() || e.failures < 3)
                && !connected.contains(&e.addr)
                && (e.manual || !connected.iter().any(|a| group(*a) == group(e.addr)))
        };
        if self.entries.iter().all(|e| e.manual) {
            return self
                .entries
                .iter()
                .filter(eligible)
                .min_by_key(|e| e.retry)
                .map(|e| e.addr);
        }
        let preferred = self
            .entries
            .iter()
            .filter(eligible)
            .any(|e| e.useful.is_some() != explorer);
        let candidates: Vec<_> = self
            .entries
            .iter()
            .filter(eligible)
            .filter(|e| !preferred || e.useful.is_some() != explorer)
            .collect();
        let mut sources = Vec::new();
        for e in &candidates {
            if !sources.contains(&e.source) {
                sources.push(e.source);
            }
        }
        if sources.is_empty() {
            return None;
        }
        let source = sources[self.rng.random_range(0..sources.len())];
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter(|e| e.source == source)
            .collect();
        Some(candidates[self.rng.random_range(0..candidates.len())].addr)
    }

    /// Reserve a failure and retry delay, returning the prior count for this attempt.
    pub fn attempted(&mut self, addr: SocketAddr, minimum: Duration) -> Option<u8> {
        if let Some(e) = self.entries.iter_mut().find(|e| e.addr == addr) {
            let previous = e.failures;
            e.failures = e.failures.saturating_add(1);
            let seconds = (30u64 << e.failures.min(7)).min(3600);
            let backoff = Duration::from_secs(self.rng.random_range(seconds / 2..=seconds));
            e.retry = Instant::now() + minimum.max(backoff);
            return Some(previous);
        }
        None
    }

    /// Clear failure pacing after service; promote only checked useful service.
    pub fn completed(&mut self, addr: SocketAddr, useful: bool) {
        let now = Instant::now();
        let can_promote = self
            .entries
            .iter()
            .filter(|e| !e.manual && e.useful.is_some())
            .count()
            < 64
            && self
                .entries
                .iter()
                .filter(|e| !e.manual && e.useful.is_some() && group(e.addr) == group(addr))
                .count()
                < 4;
        if let Some(e) = self.entries.iter_mut().find(|e| e.addr == addr) {
            e.failures = 0;
            e.retry = now;
            if useful && (e.manual || e.useful.is_some() || can_promote) {
                e.useful = Some(now);
            }
        }
    }
}
