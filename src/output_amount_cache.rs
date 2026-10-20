//! Bounded amount records, never a UTXO set or spend-status database.

use bitcoin::{Block, OutPoint};

/// Fixed allocation, including per-record provenance and parent output count.
pub(crate) const MAX_OUTPUTS: usize = 250_000;

/// A full identity is mandatory: fingerprints must never select amounts.
#[derive(Clone, Copy)]
struct OutputAmount {
    /// Transaction identity and output position.
    outpoint: OutPoint,
    /// Parent's complete output count, for detecting impossible indices.
    output_count: u32,
    /// Unvalidated amount committed by the parent txid.
    value: u64,
    /// Height whose full selected hash is held by the observer.
    height: u64,
}

/// Compile-time record budget; the observer also accounts for its own scratch.
pub(crate) const MAX_OUTPUT_ALLOCATION_BYTES: usize =
    MAX_OUTPUTS * std::mem::size_of::<OutputAmount>();

/// Sorted, fixed-capacity records with no hash-table or staging allocation.
pub(crate) struct OutputAmountCache {
    /// Allocated lazily, never grown beyond MAX_OUTPUTS.
    records: Vec<OutputAmount>,
    /// At most twenty selected heights with any retained records.
    heights: Vec<u64>,
    /// Records lost to capacity or selected-window expiration.
    pub evicted: u64,
}

impl OutputAmountCache {
    /// Start without allocating the output budget.
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            heights: Vec::with_capacity(20),
            evicted: 0,
        }
    }

    /// Discard provenance that is no longer in the selected window.
    pub fn retain(&mut self, mut keep: impl FnMut(u64) -> bool) {
        let before = self.records.len();
        self.records.retain(|record| keep(record.height));
        self.heights.retain(|height| keep(*height));
        self.evicted = self
            .evicted
            .saturating_add((before - self.records.len()) as u64);
    }

    /// Return an amount, missing parent, or a detected impossible output index.
    pub fn get(&self, outpoint: OutPoint) -> Result<Option<(u64, u64)>, ()> {
        let index = self
            .records
            .partition_point(|record| record.outpoint.txid < outpoint.txid);
        let Some(parent) = self
            .records
            .get(index)
            .filter(|r| r.outpoint.txid == outpoint.txid)
        else {
            return Ok(None);
        };
        if outpoint.vout >= parent.output_count {
            return Err(());
        }
        Ok(self
            .records
            .binary_search_by_key(&outpoint, |r| r.outpoint)
            .ok()
            .map(|i| (self.records[i].value, self.records[i].height)))
    }

    /// Cache already resident outputs, evicting whole oldest-height buckets first.
    pub fn ingest(&mut self, height: u64, block: &Block) {
        let count: usize = block.txdata.iter().map(|tx| tx.output.len()).sum();
        // Admitted blocks fit; keep the component safe for synthetic direct callers.
        if count > MAX_OUTPUTS {
            self.evicted = self.evicted.saturating_add(count as u64);
            return;
        }
        if self.records.capacity() == 0 {
            self.records = Vec::with_capacity(MAX_OUTPUTS);
        }
        while self.records.len() + count > MAX_OUTPUTS {
            let oldest = self
                .records
                .iter()
                .map(|r| r.height)
                .min()
                .expect("nonempty");
            if height <= oldest {
                // Newest-first coverage must not displace newer amounts with an
                // older incoming body. Its wanted slots were resolved already.
                self.evicted = self.evicted.saturating_add(count as u64);
                return;
            }
            self.retain(|h| h != oldest);
        }
        for tx in &block.txdata {
            let txid = tx.compute_txid();
            for (vout, output) in tx.output.iter().enumerate() {
                self.records.push(OutputAmount {
                    outpoint: OutPoint {
                        txid,
                        vout: vout as u32,
                    },
                    output_count: tx.output.len() as u32,
                    value: output.value.to_sat(),
                    height,
                });
            }
        }
        assert!(self.heights.len() < 20);
        self.heights.push(height);
        self.records.sort_unstable_by_key(|record| record.outpoint);
    }

    /// Exact current record count, not a claim of complete output coverage.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Logical allocated record bytes, excluding allocator bookkeeping and RSS.
    pub fn allocated_bytes(&self) -> usize {
        self.records.capacity() * std::mem::size_of::<OutputAmount>()
            + self.heights.capacity() * std::mem::size_of::<u64>()
    }

    /// Count actual amount buckets, independently of body/fingerprint coverage.
    pub fn blocks(&self) -> usize {
        self.heights.len()
    }
}

#[cfg(test)]
#[path = "output_amount_cache_tests.rs"]
mod tests;
