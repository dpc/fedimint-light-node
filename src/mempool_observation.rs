//! Unvalidated, bounded observations, never Bitcoin mempool acceptance.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::BuildHasher;
use std::net::SocketAddr;
use std::time::Duration;

use bitcoin::{Block, BlockHash, Txid};
use tokio::time::Instant;

/// Maximum pending full identities, independent of fingerprint storage.
pub(crate) const MAX_PENDING: usize = 50_000;
/// Maximum aggregate retained fingerprints.
const MAX_FINGERPRINTS: usize = 1_000_000;
/// New sources may extend idle life, but never the hard admission lifetime.
const IDLE_AGE: Duration = Duration::from_secs(2 * 3600);
/// Absolute lifetime even under announcements from different sources.
const HARD_AGE: Duration = Duration::from_secs(24 * 3600);

/// Fixed-size source history; a group is an address prefix, not an operator.
struct Observation {
    /// Latest actual announcer, a routing hint only; replacement never renews age.
    announcer: Option<SocketAddr>,
    /// First admission, never refreshed.
    first: Instant,
    /// Last previously unseen source, not last repeated message.
    last: Instant,
    /// At most four source prefixes; saturation does not refresh timestamps.
    sources: [Option<u64>; 4],
}

// Routing adds one inline hint, never an endpoint collection or extra index.
const _: () = assert!(std::mem::size_of::<Option<SocketAddr>>() <= 32);

/// Native diagnostics, deliberately not a synthetic Core mempool schema.
#[derive(Debug, Clone, Copy)]
pub struct MempoolObservationStatus {
    /// Retained unvalidated full txids.
    pub pending: usize,
    /// Inspected selected blocks in the current target window.
    pub covered_blocks: usize,
    /// Latest retained selected blocks targeted, at most twenty.
    pub target_blocks: usize,
    /// Retained keyed fingerprints, with possible false suppression.
    pub fingerprints: usize,
    /// Pending entries removed by capacity pressure.
    pub evicted: u64,
    /// Pending entries removed by idle or hard expiration.
    pub expired: u64,
    /// Bodies not retained because the aggregate fingerprint budget was exhausted.
    pub coverage_drops: u64,
    /// Checksummed observations dropped by staging or aggregate inbox pressure.
    pub inventory_dropped: u64,
}

/// Owner-only cache; sockets communicate through a separately bounded inbox.
pub(crate) struct MempoolObservation {
    /// Historical fee arithmetic shares admitted bodies, never download scheduling.
    fees: crate::fee_observation::FeeObservation,
    /// Full identities needed for the RPC response.
    pending: BTreeMap<Txid, Observation>,
    /// One index entry per pending transaction, never a lazy unbounded queue.
    idle: BTreeSet<(Instant, Txid)>,
    /// Independent hard expiry index.
    age: BTreeSet<(Instant, Txid)>,
    /// Process-local random hash keys; not persisted or exposed.
    hash: std::collections::hash_map::RandomState,
    /// Full selected identity prevents same-height reorg confusion.
    buckets: HashMap<(u64, BlockHash), Vec<u64>>,
    /// Current latest-twenty target, newest first.
    selected: Vec<(u64, BlockHash)>,
    /// Observable bounded-state losses.
    evicted: u64,
    /// Observable monotonic expiration.
    expired: u64,
    /// Observable fingerprint-capacity losses.
    coverage_drops: u64,
}

impl MempoolObservation {
    /// Resolve observed policy using metadata only.
    pub fn sats_per_kvb(&self) -> Option<std::num::NonZeroU64> {
        self.fees.sats_per_kvb(Instant::now())
    }
    /// Create an empty, unpersisted observation view.
    pub fn new() -> Self {
        Self {
            fees: crate::fee_observation::FeeObservation::new(),
            pending: BTreeMap::new(),
            idle: BTreeSet::new(),
            age: BTreeSet::new(),
            hash: std::collections::hash_map::RandomState::new(),
            buckets: HashMap::new(),
            selected: Vec::new(),
            evicted: 0,
            expired: 0,
            coverage_drops: 0,
        }
    }

    /// Reconcile full identities before observations, body ingestion or snapshots.
    pub fn reconcile(&mut self, selected: Vec<(u64, BlockHash)>) {
        assert!(selected.len() <= 20);
        self.fees.reconcile(&selected);
        self.buckets.retain(|key, _| selected.contains(key));
        self.selected = selected;
        // Disconnected/expired buckets never manufacture pending transactions.
    }

    /// Remove an entry and both exact indexes together.
    fn remove(&mut self, txid: Txid) {
        if let Some(entry) = self.pending.remove(&txid) {
            self.idle.remove(&(entry.last, txid));
            self.age.remove(&(entry.first, txid));
        }
    }

    /// Expire without refreshing entries on reads.
    pub fn expire(&mut self, now: Instant) {
        while let Some(&(time, txid)) = self.idle.first() {
            if now.duration_since(time) < IDLE_AGE {
                break;
            }
            self.remove(txid);
            self.expired = self.expired.saturating_add(1);
        }
        while let Some(&(time, txid)) = self.age.first() {
            if now.duration_since(time) < HARD_AGE {
                break;
            }
            self.remove(txid);
            self.expired = self.expired.saturating_add(1);
        }
    }

    /// Test approximate inclusion, never confirmation or consensus validity.
    fn suppressed(&self, txid: Txid) -> bool {
        let fingerprint = self.hash.hash_one(txid);
        self.buckets
            .values()
            .any(|bucket| bucket.binary_search(&fingerprint).is_ok())
    }

    /// Admit one checksummed legacy txid announcement under fixed capacity.
    #[cfg(test)]
    pub fn observe(&mut self, txid: Txid, source: u64, now: Instant) {
        self.observe_from(txid, source, None, now);
    }

    /// Record one bounded endpoint alongside the unchanged source-group/TTL policy.
    pub fn observe_from(
        &mut self,
        txid: Txid,
        source: u64,
        announcer: Option<SocketAddr>,
        now: Instant,
    ) {
        self.expire(now);
        if self.suppressed(txid) {
            return;
        }
        if let Some(entry) = self.pending.get_mut(&txid) {
            if announcer.is_some() {
                entry.announcer = announcer;
            }
            if !entry.sources.contains(&Some(source))
                && let Some(slot) = entry.sources.iter_mut().find(|slot| slot.is_none())
            {
                *slot = Some(source);
                self.idle.remove(&(entry.last, txid));
                entry.last = now;
                self.idle.insert((now, txid));
            }
            return;
        }
        if self.pending.len() == MAX_PENDING {
            let (_, oldest) = *self.idle.first().expect("full cache has an oldest entry");
            self.remove(oldest);
            self.evicted = self.evicted.saturating_add(1);
        }
        self.pending.insert(
            txid,
            Observation {
                announcer,
                first: now,
                last: now,
                sources: [Some(source), None, None, None],
            },
        );
        self.idle.insert((now, txid));
        self.age.insert((now, txid));
    }

    /// Distinguish absent membership from a member without an endpoint hint.
    pub fn announcer(&self, txid: bitcoin::Txid) -> Option<Option<SocketAddr>> {
        self.pending.get(&txid).map(|entry| entry.announcer)
    }

    /// Extract fingerprints and fee metadata only from an admitted selected body.
    pub fn ingest(&mut self, height: u64, block: &Block) {
        self.fees.ingest(height, block);
        let key = (height, block.block_hash());
        if !self.selected.contains(&key) || self.buckets.contains_key(&key) {
            return;
        }
        let used: usize = self.buckets.values().map(Vec::len).sum();
        if block.txdata.len() > MAX_FINGERPRINTS - used {
            self.coverage_drops = self.coverage_drops.saturating_add(1);
            return;
        }
        let mut fingerprints: Vec<_> = block
            .txdata
            .iter()
            .map(|tx| self.hash.hash_one(tx.compute_txid()))
            .collect();
        fingerprints.sort_unstable();
        fingerprints.dedup();
        self.buckets.insert(key, fingerprints);
        // Bounded scratch: at most MAX_PENDING identities, released immediately.
        let removed: Vec<_> = self
            .pending
            .keys()
            .copied()
            .filter(|txid| self.suppressed(*txid))
            .collect();
        for txid in removed {
            self.remove(txid);
        }
    }

    /// Prefer the newest missing selected body, without treating failure as fatal.
    pub fn missing(&self) -> Option<(u64, BlockHash)> {
        self.selected
            .iter()
            .copied()
            .find(|key| !self.buckets.contains_key(key))
    }

    /// Copy at most MAX_PENDING txids; callers must separately bound reply copies.
    pub fn snapshot(&self) -> Vec<Txid> {
        self.pending.keys().copied().collect()
    }

    /// Historical selected-body arithmetic, not mempool policy or an estimator.
    pub fn fee_status(&self) -> crate::fee_observation_status::FeeObservationStatus {
        self.fees
            .status(Instant::now(), self.buckets.len(), self.selected.len())
    }

    /// Report local state and losses without asserting freshness or coverage completeness.
    pub fn status(&self) -> MempoolObservationStatus {
        MempoolObservationStatus {
            pending: self.pending.len(),
            covered_blocks: self.buckets.len(),
            target_blocks: self.selected.len(),
            fingerprints: self.buckets.values().map(Vec::len).sum(),
            evicted: self.evicted,
            expired: self.expired,
            coverage_drops: self.coverage_drops,
            inventory_dropped: 0,
        }
    }
}

#[cfg(test)]
#[path = "mempool_observation_tests.rs"]
mod tests;
