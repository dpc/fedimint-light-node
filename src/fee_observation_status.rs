use bitcoin::BlockHash;

/// Historical, unvalidated arithmetic over one fixed sample, not a fee recommendation.
#[derive(Debug, Clone)]
pub struct FeeObservationStatus {
    /// Sampled selected body; None during warmup or after invalidation.
    pub block_hash: Option<BlockHash>,
    /// Height of the sampled body.
    pub height: Option<u64>,
    /// Current selected tip, not proof of network freshness.
    pub selected_tip_hash: Option<BlockHash>,
    /// Distance behind the local selected tip.
    pub blocks_behind_tip: Option<u64>,
    /// Wall-clock Unix seconds when this sample was first inspected.
    pub observed_at: Option<u64>,
    /// Monotonic elapsed age, never refreshed by reads or parent resolution.
    pub age_seconds: Option<u64>,
    /// Missing sample, non-tip sample, or at least thirty minutes old.
    pub stale: bool,
    /// Non-coinbase transactions in the inspected block.
    pub non_coinbase_tx_count: usize,
    /// Uniform fixed sample, including invalid and budget-unresolved transactions.
    pub sampled_tx_count: usize,
    /// Transactions with all amounts and valid local arithmetic.
    pub resolved_tx_count: usize,
    /// Transactions whose complete input set did not fit the slot budget.
    pub budget_unresolved_tx_count: usize,
    /// Detected invalid arithmetic or input structure, not ordinary missing data.
    pub invalid_tx_count: usize,
    /// Exact number of inputs in the fixed sample.
    pub sampled_input_count: usize,
    /// Tracked inputs with a known amount, not fraction of input value known.
    pub resolved_tracked_input_count: usize,
    /// Tracked inputs without an amount.
    pub tracked_missing_input_count: usize,
    /// Inputs not examined for amounts because their transaction exceeded the budget.
    pub untracked_input_count: usize,
    /// Unknown when any inputs were untracked.
    pub total_missing_input_count: Option<usize>,
    /// Resolved transaction percentage, not statistical confidence.
    pub resolved_percent: Option<f64>,
    /// Nearest-rank subset p25, gated on sufficient resolved observations.
    pub subset_p25_sat_per_vb: Option<f64>,
    /// Lower median of the resolved subset, subject to the same display gate.
    pub subset_p50_sat_per_vb: Option<f64>,
    /// Nearest-rank subset p75, subject to the same display gate.
    pub subset_p75_sat_per_vb: Option<f64>,
    /// Fixed-sample lower-median lower endpoint, unknown for invalid/empty samples.
    pub sample_median_lower_sat_per_vb: Option<f64>,
    /// Fixed-sample upper endpoint; None also represents an unbounded endpoint.
    pub sample_median_upper_sat_per_vb: Option<f64>,
    /// Distinguishes an unbounded upper endpoint from an unavailable bound.
    pub sample_median_upper_unbounded: bool,
    /// Whether any invalid transaction makes the sample's statistics unavailable.
    pub invalid_observation: bool,
    /// Current cached amount records, not a UTXO count.
    pub output_cache_entries: usize,
    /// Selected body buckets with cached outputs, potentially less than coverage.
    pub output_cache_blocks: usize,
    /// Records lost to cache pressure, expiration or selection invalidation.
    pub output_cache_evictions: u64,
    /// Logical output allocation, not a process RSS limit.
    pub output_cache_allocated_bytes: usize,
    /// Successfully inspected selected bodies under the existing coverage mechanism.
    pub body_coverage_complete: usize,
    /// Existing coverage target count; fee observation never changes it.
    pub body_coverage_target: usize,
}
