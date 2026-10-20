//! Zero-egress historical observations over already admitted selected bodies.
//!
//! The fixed sample includes hard-to-resolve transactions. Missing rates are not
//! zero observations; zero/infinity substitution is used only for sample bounds.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitcoin::{Block, BlockHash, OutPoint, Transaction};
use rand::RngExt;
use tokio::time::Instant;

use crate::fee_observation_status::FeeObservationStatus;
use crate::output_amount_cache::OutputAmountCache;

/// Fixed uniform sample size, independent of parent availability.
const MAX_SAMPLE: usize = 128;
/// Aggregate slots, including resolved ones, avoiding lazy/unbounded indexes.
const MAX_INPUTS: usize = 4096;
/// Bitcoin's amount bound; arithmetic here does not validate spends.
const MAX_MONEY: u64 = 21_000_000 * 100_000_000;
/// Maximum age for observed fee policy, measured from first inspection.
const STALE_AGE: Duration = Duration::from_secs(30 * 60);

/// Metadata only; no sampled transaction body is retained.
struct SampledTransaction {
    /// Sum of own outputs, checked before use.
    output_sum: u64,
    /// BIP141 weight-derived virtual bytes, not sigop-adjusted policy size.
    vsize: u64,
    /// Exact input count, also for budget-unresolved transactions.
    inputs: usize,
    /// Complete input set could not fit the slot budget.
    budget: bool,
    /// Structural or arithmetic contradiction; never counted as missing.
    invalid: bool,
}

/// Full identity and optional amount for one sampled input.
struct InputSlot {
    /// Index into the at-most-128 sampled transactions.
    owner: usize,
    /// Full txid plus output index, never a fingerprint.
    outpoint: OutPoint,
    /// Amount committed by matching parent bytes.
    value: Option<u64>,
    /// Full selected parent-body provenance, invalidated if it leaves the window.
    provenance: Option<(u64, BlockHash)>,
}

/// One active sample, retained across ordinary tip advancement until replaced.
struct ActiveSample {
    /// Full selected identity.
    key: (u64, BlockHash),
    /// Initial monotonic inspection time.
    observed: Instant,
    /// Initial wall-clock inspection time; independent of later clock jumps.
    observed_at: u64,
    /// Total eligible transactions, not just the resolved subset.
    population: usize,
    /// Fixed sample metadata.
    transactions: Vec<SampledTransaction>,
    /// Fixed-budget input slots.
    slots: Vec<InputSlot>,
}

// Compile-time enforcement before any allocation: records, cache height index,
// active metadata, input slots, reservoir/sort/rate scratch, both selected/seen
// vectors at their maximum geometric capacities, and fixed owner/status objects.
// This deliberately reserves scratch simultaneously even where lifetimes differ.
const _: () = assert!(
    crate::output_amount_cache::MAX_OUTPUT_ALLOCATION_BYTES
        + 20 * std::mem::size_of::<u64>()
        + MAX_INPUTS * (std::mem::size_of::<InputSlot>() + std::mem::size_of::<usize>())
        + MAX_SAMPLE
            * (std::mem::size_of::<SampledTransaction>() + std::mem::size_of::<usize>() + 16)
        + 64 * std::mem::size_of::<(u64, BlockHash)>()
        + std::mem::size_of::<FeeObservation>()
        + std::mem::size_of::<ActiveSample>()
        + std::mem::size_of::<FeeObservationStatus>()
        <= 16 * 1024 * 1024
);

/// Owner-local, restart-empty historical amount and sample state.
pub(crate) struct FeeObservation {
    /// Full current selected identities, newest first, at most twenty.
    selected: Vec<(u64, BlockHash)>,
    /// Prevent duplicate body delivery from redrawing or double-counting.
    seen: Vec<(u64, BlockHash)>,
    /// Bounded recent outputs, not raw transactions.
    cache: OutputAmountCache,
    /// Only one sample can await parents.
    active: Option<ActiveSample>,
}

/// Reservoir sampling uses O(MAX_SAMPLE) scratch, not an index per transaction.
fn sample_indices(count: usize, rng: &mut impl RngExt) -> Vec<usize> {
    let mut indices = Vec::with_capacity(MAX_SAMPLE);
    for index in 0..count {
        if indices.len() < MAX_SAMPLE {
            indices.push(index + 1);
        } else {
            let replacement = rng.random_range(0..=index);
            if replacement < MAX_SAMPLE {
                indices[replacement] = index + 1;
            }
        }
    }
    indices
}

/// Checked output arithmetic, separate from whole-block admission.
fn output_sum(tx: &Transaction) -> Option<u64> {
    tx.output.iter().try_fold(0u64, |sum, output| {
        sum.checked_add(output.value.to_sat())
            .filter(|sum| *sum <= MAX_MONEY)
    })
}

impl ActiveSample {
    /// Resolve exact ratios, retaining the fixed sample's unresolved membership.
    fn rates(&self) -> (Vec<(u64, u64)>, usize) {
        let mut rates = Vec::with_capacity(MAX_SAMPLE);
        let mut invalid = 0;
        for (owner, tx) in self.transactions.iter().enumerate() {
            let mut sum = Some(0u64);
            let mut missing = false;
            for slot in self.slots.iter().filter(|s| s.owner == owner) {
                if let Some(value) = slot.value {
                    sum = sum
                        .and_then(|sum| sum.checked_add(value))
                        .filter(|sum| *sum <= MAX_MONEY);
                } else {
                    missing = true;
                }
            }
            let bad = tx.invalid
                || sum.is_none()
                || tx.vsize == 0
                || (!missing && !tx.budget && sum.is_some_and(|sum| sum < tx.output_sum));
            invalid += usize::from(bad);
            if !bad && !missing && !tx.budget {
                rates.push((sum.expect("checked") - tx.output_sum, tx.vsize));
            }
        }
        rates.sort_unstable_by(|&(a, b), &(c, d)| {
            (u128::from(a) * u128::from(d)).cmp(&(u128::from(c) * u128::from(b)))
        });
        (rates, invalid)
    }

    /// Select exactly once, without looking at resolution success.
    fn new(key: (u64, BlockHash), block: &Block, rng: &mut impl RngExt) -> Self {
        let mut sample = Self {
            key,
            observed: Instant::now(),
            observed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            population: block.txdata.len().saturating_sub(1),
            transactions: Vec::with_capacity(MAX_SAMPLE),
            slots: Vec::with_capacity(MAX_INPUTS),
        };
        for index in sample_indices(sample.population, rng) {
            let tx = &block.txdata[index];
            let owner = sample.transactions.len();
            let budget = tx.input.len() > MAX_INPUTS - sample.slots.len();
            let sum = output_sum(tx);
            let mut invalid = tx.input.is_empty() || tx.output.is_empty() || sum.is_none();
            if !budget {
                let start = sample.slots.len();
                for input in &tx.input {
                    let outpoint = input.previous_output;
                    invalid |= outpoint.is_null()
                        || sample.slots[start..]
                            .iter()
                            .any(|slot| slot.outpoint == outpoint);
                    sample.slots.push(InputSlot {
                        owner,
                        outpoint,
                        value: None,
                        provenance: None,
                    });
                }
            }
            // Admitted bodies already reject null/duplicate inputs, including for
            // budget-unresolved transactions. Do not allocate a second large index.
            sample.transactions.push(SampledTransaction {
                output_sum: sum.unwrap_or(0),
                vsize: tx.weight().to_wu().div_ceil(4),
                inputs: tx.input.len(),
                budget,
                invalid,
            });
        }
        sample
    }

    /// Resolve wanted full txids in one resident-body pass, with bounded scratch.
    fn resolve_body(&mut self, height: u64, block: &Block) {
        let key = (height, block.block_hash());
        // Sort slot indices by full outpoint. Binary ranges avoid scanning 4096
        // slots for every output in an adversarial block.
        let mut wanted: Vec<_> = (0..self.slots.len()).collect();
        wanted.sort_unstable_by_key(|&i| self.slots[i].outpoint);
        for tx in &block.txdata {
            let txid = tx.compute_txid();
            let start = wanted.partition_point(|&i| self.slots[i].outpoint.txid < txid);
            let end = wanted.partition_point(|&i| self.slots[i].outpoint.txid <= txid);
            for &index in &wanted[start..end] {
                let slot = &mut self.slots[index];
                let value = tx
                    .output
                    .get(slot.outpoint.vout as usize)
                    .map(|o| o.value.to_sat());
                if let Some(value) = value.filter(|v| *v <= MAX_MONEY) {
                    if slot.value.is_some_and(|old| old != value) {
                        self.transactions[slot.owner].invalid = true;
                    }
                    slot.value = Some(value);
                    slot.provenance = Some(key);
                } else {
                    self.transactions[slot.owner].invalid = true;
                }
            }
        }
    }

    /// Resolve cached parents before the new body can evict useful old amounts.
    fn resolve_cache(&mut self, cache: &OutputAmountCache, selected: &[(u64, BlockHash)]) {
        for slot in &mut self.slots {
            match cache.get(slot.outpoint) {
                Ok(Some((value, height))) if value <= MAX_MONEY => {
                    slot.value = Some(value);
                    slot.provenance = selected.iter().find(|(h, _)| *h == height).copied();
                }
                Ok(None) => {}
                _ => self.transactions[slot.owner].invalid = true,
            }
        }
    }
}

impl FeeObservation {
    /// Read the exact finite upper endpoint without fetching or refreshing state.
    pub fn sats_per_kvb(&self, now: Instant) -> Option<std::num::NonZeroU64> {
        let sample = self.active.as_ref()?;
        if self.selected.first() != Some(&sample.key)
            || now.saturating_duration_since(sample.observed) >= STALE_AGE
            || sample.transactions.is_empty()
        {
            return None;
        }
        let (rates, invalid) = sample.rates();
        if invalid != 0
            || !(rates.len() >= 32 || (sample.population < 32 && rates.len() == sample.population))
        {
            return None;
        }
        let &(fee, vsize) = rates.get(sample.transactions.len().div_ceil(2) - 1)?;
        exact_sats_per_kvb(fee, vsize)
    }

    /// No allocations for output records until an admitted body arrives.
    pub fn new() -> Self {
        Self {
            selected: Vec::new(),
            seen: Vec::new(),
            cache: OutputAmountCache::new(),
            active: None,
        }
    }

    /// Drop all amount provenance on a fork or discontinuous selection change.
    pub fn reconcile(&mut self, selected: &[(u64, BlockHash)]) {
        assert!(selected.len() <= 20);
        if selected == self.selected {
            return;
        }
        let changed_branch = !self.selected.is_empty()
            && (self
                .selected
                .iter()
                .any(|(height, hash)| selected.iter().any(|(h, b)| h == height && b != hash))
                || !self.selected.iter().any(|key| selected.contains(key)));
        if changed_branch {
            self.cache.retain(|_| false);
            self.seen.clear();
            self.active = None;
        }
        self.cache
            .retain(|height| selected.iter().any(|(h, _)| *h == height));
        self.seen.retain(|key| selected.contains(key));
        if self.active.as_ref().is_some_and(|a| {
            !selected.contains(&a.key)
                || a.slots
                    .iter()
                    .any(|s| s.provenance.is_some_and(|key| !selected.contains(&key)))
        }) {
            self.active = None;
        }
        self.selected.clear();
        self.selected.extend_from_slice(selected);
    }

    /// Inspect only bodies delivered by the existing observer; never request any.
    pub fn ingest(&mut self, height: u64, block: &Block) {
        self.ingest_with_rng(height, block, &mut rand::rng());
    }

    /// Inject randomness for deterministic sampling tests.
    fn ingest_with_rng(&mut self, height: u64, block: &Block, rng: &mut impl RngExt) {
        let key = (height, block.block_hash());
        if !self.selected.contains(&key) || self.seen.contains(&key) {
            return;
        }
        if self
            .active
            .as_ref()
            .is_none_or(|active| height > active.key.0)
        {
            // Release the previous sample before allocating its successor.
            self.active = None;
            let mut sample = ActiveSample::new(key, block, rng);
            sample.resolve_cache(&self.cache, &self.selected);
            self.active = Some(sample);
        }
        if let Some(sample) = &mut self.active {
            sample.resolve_body(height, block);
        }
        self.cache.ingest(height, block);
        self.seen.push(key);
        let status = self.status(Instant::now(), 0, self.selected.len());
        tracing::debug!(event = "fee_observation", height, hash = %key.1,
            sampled = status.sampled_tx_count, resolved = status.resolved_tx_count,
            missing = status.tracked_missing_input_count, invalid = status.invalid_tx_count,
            "historical fee sample updated");
    }

    /// Copy only a fixed-size summary; reads never redraw or refresh sample age.
    pub fn status(&self, now: Instant, complete: usize, target: usize) -> FeeObservationStatus {
        let active = self.active.as_ref();
        let tip = self.selected.first();
        let (rates, invalid) = active.map_or_else(|| (Vec::new(), 0), ActiveSample::rates);
        let mut budget = 0;
        let mut inputs = 0;
        let mut untracked = 0;
        if let Some(sample) = active {
            for tx in &sample.transactions {
                inputs += tx.inputs;
                budget += usize::from(tx.budget);
                if tx.budget {
                    untracked += tx.inputs;
                }
            }
        }
        let display = |index: usize| rates[index].0 as f64 / rates[index].1 as f64;
        let sampled = active.map_or(0, |s| s.transactions.len());
        let resolved = rates.len();
        let population = active.map_or(0, |s| s.population);
        let gate = invalid == 0 && (resolved >= 32 || (population < 32 && resolved == population));
        let quantile = |numerator: usize, denominator: usize| {
            (gate && resolved > 0)
                .then(|| display((resolved * numerator).div_ceil(denominator) - 1))
        };
        let rank = sampled.div_ceil(2);
        let bounds = sampled > 0 && invalid == 0;
        let missing = sampled - resolved;
        let lower = bounds.then(|| {
            if rank <= missing {
                0.0
            } else {
                display(rank - missing - 1)
            }
        });
        let upper = (bounds && rank <= resolved).then(|| display(rank - 1));
        let known = active.map_or(0, |s| {
            s.slots.iter().filter(|slot| slot.value.is_some()).count()
        });
        let tracked_missing = active.map_or(0, |s| s.slots.len()) - known;
        let age = active.map(|s| now.saturating_duration_since(s.observed));
        let lag = active
            .zip(tip)
            .map(|(s, tip)| tip.0.saturating_sub(s.key.0));
        FeeObservationStatus {
            block_hash: active.map(|s| s.key.1),
            height: active.map(|s| s.key.0),
            selected_tip_hash: tip.map(|key| key.1),
            blocks_behind_tip: lag,
            observed_at: active.map(|s| s.observed_at),
            age_seconds: age.map(|age| age.as_secs()),
            stale: age.is_none_or(|age| age >= STALE_AGE)
                || lag != Some(0)
                || active.map(|s| s.key.1) != tip.map(|key| key.1),
            non_coinbase_tx_count: population,
            sampled_tx_count: sampled,
            resolved_tx_count: resolved,
            budget_unresolved_tx_count: budget,
            invalid_tx_count: invalid,
            sampled_input_count: inputs,
            resolved_tracked_input_count: known,
            tracked_missing_input_count: tracked_missing,
            untracked_input_count: untracked,
            total_missing_input_count: (untracked == 0).then_some(tracked_missing),
            resolved_percent: (sampled > 0).then(|| resolved as f64 * 100.0 / sampled as f64),
            subset_p25_sat_per_vb: quantile(1, 4),
            subset_p50_sat_per_vb: quantile(1, 2),
            subset_p75_sat_per_vb: quantile(3, 4),
            sample_median_lower_sat_per_vb: lower,
            sample_median_upper_sat_per_vb: upper,
            sample_median_upper_unbounded: bounds && rank > resolved,
            invalid_observation: invalid != 0,
            output_cache_entries: self.cache.len(),
            output_cache_blocks: self.cache.blocks(),
            output_cache_evictions: self.cache.evicted,
            output_cache_allocated_bytes: self.cache.allocated_bytes(),
            body_coverage_complete: complete,
            body_coverage_target: target,
        }
    }
}

/// Round the selected exact ratio up once, before the BTC wire conversion.
fn exact_sats_per_kvb(fee: u64, vsize: u64) -> Option<std::num::NonZeroU64> {
    if vsize == 0 {
        return None;
    }
    let value = u128::from(fee)
        .checked_mul(1000)?
        .div_ceil(u128::from(vsize));
    std::num::NonZeroU64::new(u64::try_from(value).ok()?)
}

#[cfg(test)]
#[path = "fee_observation_tests.rs"]
mod tests;
