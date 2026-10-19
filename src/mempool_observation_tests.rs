use bitcoin::Network;
use bitcoin::blockdata::constants::genesis_block;
use bitcoin::hashes::Hash;

use super::*;

fn txid(n: u64) -> Txid {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    Txid::from_byte_array(bytes)
}

#[test]
fn repeated_source_does_not_refresh_and_reads_do_not_refresh() {
    let mut cache = MempoolObservation::new();
    let now = Instant::now();
    cache.observe(txid(1), 42, now);
    cache.observe(txid(1), 42, now + IDLE_AGE / 2);
    assert_eq!(cache.snapshot(), vec![txid(1)]);
    cache.expire(now + IDLE_AGE);
    assert!(cache.snapshot().is_empty());
    assert_eq!(cache.status().expired, 1);
}

#[test]
fn distinct_sources_saturate_and_hard_age_is_independent() {
    let mut cache = MempoolObservation::new();
    let now = Instant::now();
    for source in 0..5 {
        cache.observe(txid(1), source, now + Duration::from_secs(source));
    }
    let entry = cache.pending.get(&txid(1)).unwrap();
    assert_eq!(entry.sources, [Some(0), Some(1), Some(2), Some(3)]);
    assert_eq!(entry.last, now + Duration::from_secs(3));
    // Force an idle refresh near the hard deadline to independently test that index.
    let entry = cache.pending.get_mut(&txid(1)).unwrap();
    cache.idle.remove(&(entry.last, txid(1)));
    entry.last = now + HARD_AGE - Duration::from_secs(1);
    cache.idle.insert((entry.last, txid(1)));
    cache.expire(now + HARD_AGE);
    assert!(cache.pending.is_empty());
    assert!(cache.idle.is_empty());
    assert!(cache.age.is_empty());
}

#[test]
fn deterministic_lru_capacity_has_no_unbounded_stale_index() {
    let mut cache = MempoolObservation::new();
    let now = Instant::now();
    for n in 0..MAX_PENDING as u64 {
        cache.observe(txid(n), 0, now + Duration::from_micros(n));
    }
    cache.observe(txid(0), 0, now + Duration::from_secs(1));
    cache.observe(txid(1), 1, now + Duration::from_secs(1));
    cache.observe(txid(MAX_PENDING as u64), 0, now + Duration::from_secs(2));
    assert!(!cache.pending.contains_key(&txid(0)));
    assert!(cache.pending.contains_key(&txid(1)));
    assert_eq!(cache.pending.len(), MAX_PENDING);
    assert_eq!(cache.idle.len(), MAX_PENDING);
    assert_eq!(cache.age.len(), MAX_PENDING);
    assert_eq!(cache.evicted, 1);
}

#[test]
fn same_height_reorg_and_window_expiry_never_resurrect() {
    let mut cache = MempoolObservation::new();
    let block = genesis_block(Network::Bitcoin);
    let confirmed = block.txdata[0].compute_txid();
    let now = Instant::now();
    cache.reconcile(vec![(0, block.block_hash())]);
    cache.observe(confirmed, 1, now);
    cache.ingest(0, &block);
    assert!(cache.snapshot().is_empty());
    assert_eq!(cache.status().covered_blocks, 1);
    cache.observe(confirmed, 2, now);
    assert!(cache.snapshot().is_empty());
    cache.reconcile(vec![(0, BlockHash::all_zeros())]);
    assert_eq!(cache.status().covered_blocks, 0);
    assert!(cache.snapshot().is_empty());
    cache.observe(confirmed, 1, now);
    assert_eq!(cache.snapshot(), vec![confirmed]);
    cache.reconcile(vec![(0, block.block_hash())]);
    cache.ingest(0, &block);
    cache.reconcile(vec![(20, BlockHash::all_zeros())]);
    assert!(cache.snapshot().is_empty());
}

#[test]
fn aggregate_fingerprint_budget_does_not_claim_coverage() {
    let mut cache = MempoolObservation::new();
    let block = genesis_block(Network::Bitcoin);
    let other = (1, BlockHash::all_zeros());
    cache.reconcile(vec![(0, block.block_hash()), other]);
    cache.buckets.insert(other, vec![0; MAX_FINGERPRINTS]);
    cache.ingest(0, &block);
    assert_eq!(cache.status().covered_blocks, 1);
    assert_eq!(cache.status().coverage_drops, 1);
    assert_eq!(cache.missing(), Some((0, block.block_hash())));
}
