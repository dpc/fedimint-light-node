//! Synthetic arithmetic tests start AFTER mechanical body admission. They do not
//! claim that these synthetic bodies pass mainnet header or consensus validation.

use bitcoin::blockdata::constants::genesis_block;
use bitcoin::hashes::Hash;
use bitcoin::{Amount, Network, ScriptBuf, TxIn, TxOut, Txid};
use rand::{SeedableRng, rngs::StdRng};

use super::*;

fn parent(value: u64, tag: u32) -> Transaction {
    let mut tx = genesis_block(Network::Bitcoin).txdata.remove(0);
    tx.lock_time = bitcoin::absolute::LockTime::from_consensus(tag);
    tx.output = vec![TxOut {
        value: Amount::from_sat(value),
        script_pubkey: ScriptBuf::new(),
    }];
    tx
}

fn spend(parent: &Transaction, value: u64) -> Transaction {
    let mut tx = parent.clone();
    tx.input = vec![TxIn {
        previous_output: OutPoint {
            txid: parent.compute_txid(),
            vout: 0,
        },
        ..TxIn::default()
    }];
    tx.output[0].value = Amount::from_sat(value);
    tx
}

fn body(transactions: Vec<Transaction>, tag: u32) -> Block {
    let mut block = genesis_block(Network::Bitcoin);
    block.header.nonce = tag;
    block.txdata.extend(transactions);
    block
}

fn status(observer: &FeeObservation) -> FeeObservationStatus {
    observer.status(Instant::now(), 0, 0)
}

#[test]
fn warmup_same_body_zero_fee_duplicate_and_age() {
    let mut observer = FeeObservation::new();
    assert!(status(&observer).block_hash.is_none());
    assert!(status(&observer).stale);
    let p = parent(10, 1);
    let child = spend(&p, 10);
    // Parent is represented as a non-coinbase synthetic transaction here, so its
    // null input intentionally makes this sample invalid. Put it in coinbase position.
    let mut block = body(vec![child], 1);
    block.txdata[0] = p;
    observer.reconcile(&[(5, block.block_hash())]);
    observer.ingest(5, &block);
    let s = status(&observer);
    assert_eq!(s.sampled_tx_count, 1);
    assert_eq!(s.resolved_tx_count, 1);
    assert_eq!(s.subset_p50_sat_per_vb, Some(0.0));
    assert_eq!(s.sample_median_lower_sat_per_vb, Some(0.0));
    assert_eq!(s.sample_median_upper_sat_per_vb, Some(0.0));
    let before = observer.active.as_ref().unwrap().observed;
    observer.ingest(5, &block);
    assert_eq!(observer.active.as_ref().unwrap().observed, before);
    assert_eq!(status(&observer).output_cache_entries, 2);
    assert!(observer.status(before + STALE_AGE, 0, 0).stale);
}

#[test]
fn later_parent_resolves_without_replacing_sample_or_refreshing_age() {
    let p = parent(1000, 2);
    let mut old = body(vec![], 2);
    old.txdata[0] = p.clone();
    let new = body(vec![spend(&p, 900)], 3);
    let mut observer = FeeObservation::new();
    observer.reconcile(&[(11, new.block_hash()), (10, old.block_hash())]);
    observer.ingest(11, &new);
    let s = status(&observer);
    assert_eq!(s.resolved_tx_count, 0);
    assert_eq!(s.tracked_missing_input_count, 1);
    assert_eq!(s.sample_median_lower_sat_per_vb, Some(0.0));
    assert!(s.sample_median_upper_unbounded);
    let observed = observer.active.as_ref().unwrap().observed;
    observer.ingest(10, &old);
    let s = status(&observer);
    assert_eq!(s.resolved_tx_count, 1);
    assert_eq!(s.total_missing_input_count, Some(0));
    assert_eq!(s.block_hash, Some(new.block_hash()));
    assert_eq!(observer.active.as_ref().unwrap().observed, observed);
    assert_eq!(
        s.sample_median_lower_sat_per_vb,
        s.sample_median_upper_sat_per_vb
    );
}

#[test]
fn cached_parent_tip_advance_reorg_and_prune() {
    let p = parent(1000, 4);
    let mut old = body(vec![], 4);
    old.txdata[0] = p.clone();
    let new = body(vec![spend(&p, 900)], 5);
    let tip = body(vec![], 6);
    let mut observer = FeeObservation::new();
    observer.reconcile(&[(10, old.block_hash())]);
    observer.ingest(10, &old);
    observer.reconcile(&[(11, new.block_hash()), (10, old.block_hash())]);
    observer.ingest(11, &new);
    assert_eq!(status(&observer).resolved_tx_count, 1);
    observer.reconcile(&[
        (12, tip.block_hash()),
        (11, new.block_hash()),
        (10, old.block_hash()),
    ]);
    assert_eq!(status(&observer).blocks_behind_tip, Some(1));
    assert!(status(&observer).stale);
    assert_eq!(status(&observer).resolved_tx_count, 1);
    // Same-height replacement invalidates even if the sample block is unchanged:
    // any amount may have come from the replaced provenance.
    observer.reconcile(&[
        (12, tip.block_hash()),
        (11, new.block_hash()),
        (10, tip.block_hash()),
    ]);
    assert!(status(&observer).block_hash.is_none());
    assert_eq!(status(&observer).output_cache_entries, 0);
    observer.ingest(11, &new);
    assert_eq!(status(&observer).resolved_tx_count, 0);
    observer.reconcile(&[(12, tip.block_hash())]);
    assert!(status(&observer).block_hash.is_none());
}

#[test]
fn deterministic_uniform_reservoir_has_no_duplicates_or_resolution_bias() {
    let mut a = StdRng::seed_from_u64(42);
    let mut b = StdRng::seed_from_u64(42);
    let sample = sample_indices(1000, &mut a);
    assert_eq!(sample, sample_indices(1000, &mut b));
    assert_eq!(sample.len(), 128);
    let mut unique = sample.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 128);
    assert!(sample.iter().all(|&i| (1..=1000).contains(&i)));
    assert_eq!(sample_indices(3, &mut a), vec![1, 2, 3]);
    // Every position must appear under independent deterministic seeds; this
    // catches an accidental prefix-only sample without a flaky significance test.
    let mut seen = [false; 256];
    for seed in 0..32 {
        for index in sample_indices(256, &mut StdRng::seed_from_u64(seed)) {
            seen[index - 1] = true;
        }
    }
    assert!(seen.into_iter().all(|s| s));
}

#[test]
fn many_inputs_are_budget_unresolved_not_replaced_or_reported_missing() {
    let p = parent(100, 7);
    let mut child = spend(&p, 0);
    child.input = (0..=MAX_INPUTS)
        .map(|vout| TxIn {
            previous_output: OutPoint {
                txid: p.compute_txid(),
                vout: vout as u32,
            },
            ..TxIn::default()
        })
        .collect();
    let block = body(vec![child], 7);
    let mut observer = FeeObservation::new();
    observer.reconcile(&[(1, block.block_hash())]);
    observer.ingest(1, &block);
    let s = status(&observer);
    assert_eq!(s.sampled_tx_count, 1);
    assert_eq!(s.budget_unresolved_tx_count, 1);
    assert_eq!(s.sampled_input_count, 4097);
    assert_eq!(s.untracked_input_count, 4097);
    assert_eq!(s.tracked_missing_input_count, 0);
    assert_eq!(s.total_missing_input_count, None);
    assert!(s.sample_median_upper_unbounded);
    assert_eq!(s.sample_median_lower_sat_per_vb, Some(0.0));
}

#[test]
fn invalid_arithmetic_indices_null_and_duplicates_disable_statistics() {
    let p = parent(100, 8);
    let negative = spend(&p, 101);
    let mut duplicate = spend(&p, 1);
    duplicate.input.push(duplicate.input[0].clone());
    let mut null = spend(&p, 1);
    null.input[0].previous_output = OutPoint::null();
    let mut index = spend(&p, 1);
    index.input[0].previous_output.vout = 1;
    let mut overflow = spend(&p, MAX_MONEY + 1);
    overflow.input[0].previous_output.txid = Txid::from_byte_array([4; 32]);
    for child in [negative, duplicate, null, index, overflow] {
        let mut block = body(vec![child], 8);
        block.txdata[0] = p.clone();
        let mut observer = FeeObservation::new();
        observer.reconcile(&[(1, block.block_hash())]);
        observer.ingest(1, &block);
        let s = status(&observer);
        assert_eq!(s.invalid_tx_count, 1);
        assert!(s.invalid_observation);
        assert!(s.subset_p50_sat_per_vb.is_none());
        assert!(s.sample_median_lower_sat_per_vb.is_none());
        assert!(!s.sample_median_upper_unbounded);
    }
}

#[test]
fn partial_quantiles_and_fixed_sample_lower_median_bounds() {
    let p = parent(1000, 9);
    let mut block = body(
        vec![
            spend(&p, 990),
            spend(&p, 980),
            spend(&p, 970),
            spend(&p, 960),
        ],
        9,
    );
    block.txdata[0] = p;
    // Make one missing. Three resolved samples in a four-tx block are below gate.
    block.txdata[4].input[0].previous_output.txid = Txid::from_byte_array([9; 32]);
    let mut observer = FeeObservation::new();
    observer.reconcile(&[(1, block.block_hash())]);
    observer.ingest(1, &block);
    let s = status(&observer);
    let size = block.txdata[1].weight().to_wu().div_ceil(4) as f64;
    assert_eq!(s.resolved_tx_count, 3);
    assert_eq!(s.resolved_percent, Some(75.0));
    assert!(s.subset_p50_sat_per_vb.is_none());
    assert_eq!(s.sample_median_lower_sat_per_vb, Some(10.0 / size));
    assert_eq!(s.sample_median_upper_sat_per_vb, Some(20.0 / size));
}

#[test]
fn logical_allocation_bound_includes_slots_scratch_and_indexes() {
    let mut observer = FeeObservation::new();
    let block = body(vec![], 10);
    observer.reconcile(&[(1, block.block_hash())]);
    observer.ingest(1, &block);
    let maximum = observer.cache.allocated_bytes()
        + MAX_INPUTS * (std::mem::size_of::<InputSlot>() + std::mem::size_of::<usize>())
        + MAX_SAMPLE
            * (std::mem::size_of::<SampledTransaction>() + std::mem::size_of::<usize>() + 8)
        + 64 * std::mem::size_of::<(u64, BlockHash)>()
        + std::mem::size_of::<FeeObservation>()
        + std::mem::size_of::<ActiveSample>()
        + std::mem::size_of::<FeeObservationStatus>();
    assert!(maximum <= 16 * 1024 * 1024, "{maximum}");
}

#[test]
fn observer_integration_preserves_coverage_priority_and_later_parent_resolution() {
    // Explicit post-admission synthetic boundary, NOT mainnet P2P admission.
    let p = parent(1000, 11);
    let mut older = body(vec![], 11);
    older.txdata[0] = p.clone();
    let newer = body(vec![spend(&p, 900)], 12);
    let mut observer = crate::mempool_observation::MempoolObservation::new();
    observer.reconcile(vec![(2, newer.block_hash()), (1, older.block_hash())]);
    assert_eq!(observer.missing(), Some((2, newer.block_hash())));
    observer.ingest(2, &newer);
    assert_eq!(observer.fee_status().resolved_tx_count, 0);
    assert_eq!(observer.fee_status().body_coverage_complete, 1);
    assert_eq!(observer.missing(), Some((1, older.block_hash())));
    observer.ingest(1, &older);
    assert_eq!(observer.fee_status().resolved_tx_count, 1);
    assert_eq!(observer.fee_status().body_coverage_complete, 2);
    assert_eq!(observer.missing(), None);
    // Losing retained parent provenance discards the sample, not just its cache.
    observer.reconcile(vec![(2, newer.block_hash())]);
    assert!(observer.fee_status().block_hash.is_none());
}

#[test]
fn bounds_match_explicit_zero_and_infinity_completion_for_odd_even_samples() {
    let key = (1, genesis_block(Network::Bitcoin).block_hash());
    for count in (0..=16).chain([31, 32, 33, 127, 128]) {
        for resolved in 0..=count {
            let mut observer = FeeObservation::new();
            observer.active = Some(ActiveSample {
                key,
                observed: Instant::now(),
                observed_at: 0,
                population: count,
                transactions: (0..count)
                    .map(|_| SampledTransaction {
                        output_sum: 0,
                        vsize: 1,
                        inputs: 1,
                        budget: false,
                        invalid: false,
                    })
                    .collect(),
                slots: (0..count)
                    .map(|owner| InputSlot {
                        owner,
                        outpoint: OutPoint::null(),
                        value: (owner < resolved).then_some((owner * 2) as u64),
                        provenance: None,
                    })
                    .collect(),
            });
            let s = status(&observer);
            if count == 0 {
                assert!(s.sample_median_lower_sat_per_vb.is_none());
                assert!(s.sample_median_upper_sat_per_vb.is_none());
                assert!(!s.sample_median_upper_unbounded);
                continue;
            }
            let known: Vec<f64> = (0..resolved).map(|i| (i * 2) as f64).collect();
            let mut lows = known.clone();
            lows.resize(count, 0.0);
            lows.sort_unstable_by(f64::total_cmp);
            let mut highs = known.clone();
            highs.resize(count, f64::INFINITY);
            highs.sort_unstable_by(f64::total_cmp);
            let median_index = (count - 1) / 2;
            assert_eq!(s.sample_median_lower_sat_per_vb, Some(lows[median_index]));
            assert_eq!(
                s.sample_median_upper_sat_per_vb,
                highs[median_index]
                    .is_finite()
                    .then_some(highs[median_index])
            );
            assert_eq!(
                s.sample_median_upper_unbounded,
                highs[median_index].is_infinite()
            );
            let display = resolved > 0 && (resolved >= 32 || resolved == count);
            assert_eq!(
                s.subset_p50_sat_per_vb,
                display.then(|| known[(resolved - 1) / 2])
            );
            assert_eq!(
                s.subset_p25_sat_per_vb,
                display.then(|| known[(resolved - 1) / 4])
            );
            assert_eq!(
                s.subset_p75_sat_per_vb,
                display.then(|| known[(resolved * 3 - 1) / 4])
            );
        }
    }
}

#[test]
fn checked_input_aggregate_and_inconsistent_amounts_invalidate() {
    let p = parent(MAX_MONEY, 20);
    let q = parent(MAX_MONEY, 21);
    let mut child = spend(&p, 0);
    child.input.push(TxIn {
        previous_output: OutPoint {
            txid: q.compute_txid(),
            vout: 0,
        },
        ..TxIn::default()
    });
    let block = body(vec![child], 22);
    let mut sample = ActiveSample::new(
        (3, block.block_hash()),
        &block,
        &mut StdRng::seed_from_u64(1),
    );
    let mut parents = body(vec![q], 23);
    parents.txdata[0] = p;
    sample.resolve_body(2, &parents);
    let mut observer = FeeObservation::new();
    observer.active = Some(sample);
    assert_eq!(status(&observer).invalid_tx_count, 1);
    // A full-identity match must not silently overwrite an inconsistent amount.
    let sample = observer.active.as_mut().unwrap();
    sample.slots[0].value = Some(1);
    sample.resolve_body(2, &parents);
    assert!(sample.transactions[0].invalid);
}

#[test]
fn capacity_eviction_preserves_resolved_slots_but_disjoint_selection_clears_them() {
    let p = parent(1000, 30);
    let mut old = body(vec![], 30);
    old.txdata[0] = p.clone();
    let newest = body(vec![spend(&p, 900)], 31);
    let fillers: Vec<_> = (32..35)
        .map(|tag| {
            let mut block = body(vec![], tag);
            block.txdata[0].lock_time = bitcoin::absolute::LockTime::from_consensus(tag);
            block.txdata[0].output = vec![
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new(),
                };
                100_000
            ];
            block
        })
        .collect();
    let selected = vec![
        (20, newest.block_hash()),
        (12, fillers[2].block_hash()),
        (11, fillers[1].block_hash()),
        (10, fillers[0].block_hash()),
        (1, old.block_hash()),
    ];
    let mut observer = FeeObservation::new();
    observer.reconcile(&selected);
    observer.ingest(1, &old);
    observer.ingest(20, &newest);
    assert_eq!(status(&observer).resolved_tx_count, 1);
    for (index, filler) in fillers.iter().enumerate() {
        observer.ingest(10 + index as u64, filler);
    }
    assert_eq!(
        observer.cache.get(OutPoint {
            txid: p.compute_txid(),
            vout: 0
        }),
        Ok(None)
    );
    assert_eq!(status(&observer).resolved_tx_count, 1);
    assert_eq!(status(&observer).block_hash, Some(newest.block_hash()));
    let replacement = body(vec![], 99);
    observer.reconcile(&[(100, replacement.block_hash())]);
    assert!(status(&observer).block_hash.is_none());
    assert_eq!(status(&observer).output_cache_entries, 0);
    assert!(observer.seen.is_empty());
    observer.ingest(100, &replacement);
    assert_eq!(status(&observer).block_hash, Some(replacement.block_hash()));
}
