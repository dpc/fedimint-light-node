use std::time::{Duration, Instant};

use fedimint_light_node::header_sync::HeaderSync;

/// Report selection changes immediately and unchanged state at most once a minute.
#[derive(Default)]
pub struct SyncProgress {
    /// Last reported selection generation.
    generation: Option<u64>,
    /// Last summary time; peer churn alone must not produce info-level floods.
    reported: Option<Instant>,
    /// Last degradation warning; repeated failures or flapping cannot flood stderr.
    warned: Option<Instant>,
}

impl SyncProgress {
    /// Emit bounded summaries without claiming freshness or consensus validation.
    pub fn report(
        &mut self,
        sync: &HeaderSync,
    ) -> Result<(), fedimint_light_node::store_error::StoreError> {
        let status = sync.status()?;
        let (connections, candidates, cache_failed) = sync.peer_counts();
        let now = Instant::now();
        if self.warning_due(connections, cache_failed, now) {
            tracing::warn!(
                event = "sync_degraded",
                connections,
                candidates,
                cache_failed,
                "peer availability or disposable cache degraded; retrying"
            );
        }
        if self.summary_due(status.generation, now) {
            tracing::info!(event = "sync_progress", selected_height = status.height,
                selected_hash = %status.hash, generation = status.generation,
                connections, candidates, cache_failed, stopped = status.stopped,
                freshness = "unproven", "selected chain summary");
        }
        Ok(())
    }

    /// Keep degradation warnings bounded even when connections repeatedly recover.
    fn warning_due(&mut self, connections: usize, cache_failed: bool, now: Instant) -> bool {
        if (connections == 0 || cache_failed)
            && self
                .warned
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(60))
        {
            self.warned = Some(now);
            true
        } else {
            false
        }
    }

    /// Commit a summary deadline independently of how often callers visit.
    fn summary_due(&mut self, generation: u64, now: Instant) -> bool {
        if self.generation == Some(generation)
            && self
                .reported
                .is_some_and(|last| now.duration_since(last) < Duration::from_secs(60))
        {
            return false;
        }
        self.generation = Some(generation);
        self.reported = Some(now);
        true
    }
}

#[cfg(test)]
#[path = "sync_progress_tests.rs"]
mod tests;
