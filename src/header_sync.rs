use std::net::SocketAddr;

use bitcoin::hashes::Hash;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::GetHeadersMessage;
use bitcoin::{Block, BlockHash, Target, Transaction, block::Header};
use tokio::time::timeout;

use crate::block_download::download;
use crate::block_download_config::BlockDownloadConfig;
use crate::block_download_error::BlockDownloadError;
use crate::declared_pow::check_declared_pow;
use crate::fee_policy::FeePolicy;
use crate::header_error::HeaderError;
use crate::header_wire::{Message, send};
use crate::peer_connection::PeerConnection;
use crate::peer_pool::PeerPool;
use crate::store_error::StoreError;
use crate::sync_config::SyncConfig;
use crate::sync_error::SyncError;
use crate::transaction_submission::submit;
use crate::transaction_submission_config::TransactionSubmissionConfig;
use crate::transaction_submission_report::{
    TransactionSubmissionError, TransactionSubmissionReport,
};
use crate::validated_header_store::ValidatedHeaderStore;

#[cfg(test)]
#[path = "explorer_sync_tests.rs"]
mod explorer_tests;

/// Caller-driven mainnet v1 header synchronization from an initialized trusted store.
///
/// Each paced `step` leases a peer from the maintained outbound pool. Bounded idle
/// drivers service other connections; DNS and address hints supplement optional
/// manual peers. The caller supplies a Tokio runtime with I/O and time enabled.
/// No runtime, listener or global configuration is installed. One foreground
/// candidate and body decoder avoid multiplicative per-peer download budgets.
/// Explicit explorer configuration adds separately bounded reverse-header cursors.
///
/// Dropping the `step` future closes its socket and drops any unselected candidate.
/// Other idle connections remain maintained. Already committed progress remains observable
/// through `status`, including when a later request fails or gets cancelled.
/// Synchronous bounded validation and redb transactions run between awaits:
/// they cannot be interrupted and may block the executor. Use a caller-owned
/// dedicated runtime thread if that latency is unacceptable.
pub struct HeaderSync {
    /// Exclusive validated state; never held in a transaction across an await.
    store: ValidatedHeaderStore,
    /// Validated global resource and time budgets.
    config: SyncConfig,
    /// Sole owner of candidates, maintained sockets and bounded idle drivers.
    pool: PeerPool,
    /// Monotonic in-memory selection generation, not a persisted event log.
    generation: u64,
    /// Terminal local/recovery failures require explicit caller intervention.
    stopped: bool,
    /// Operator fee information, independent of selected headers and persistence.
    fee_policy: FeePolicy,
    /// Optional in-memory unvalidated transaction observations.
    mempool: Option<crate::mempool_observation::MempoolObservation>,
    /// Fixed-minute background coverage reservation window.
    coverage_window: tokio::time::Instant,
    /// Background receive allowance; whole attempts reserved even on cancellation.
    coverage_bytes: usize,
    /// Explicit opt-in HTTP evidence caches, independent of the P2P candidate.
    explorers: Option<crate::explorer_headers::ExplorerHeaders>,
    /// P2P pacing survives cancellation of a short owner maintenance slice.
    peer_start: Option<tokio::time::Instant>,
}

/// Selected state, not a freshness claim or a consensus-valid chain guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncStatus {
    /// Explicit operator policy, not an inferred network estimate.
    pub fee_policy: FeePolicy,
    /// Current selected height.
    pub height: u64,
    /// Current selected hash.
    pub hash: BlockHash,
    /// Number of canonical updates in this service instance; resets on restart.
    pub generation: u64,
    /// A terminal error has stopped further visits.
    pub stopped: bool,
}

impl SyncStatus {
    /// Count selected blocks including height zero, without a finality subtraction.
    ///
    /// A supported reorganization may reduce this count. Neither reading it nor
    /// fetching a block protects history or proves freshness.
    pub fn block_count(&self) -> Result<u64, StoreError> {
        self.height
            .checked_add(1)
            .ok_or(StoreError::InvalidUpdate("selected block count overflow"))
    }
}

/// One bounded peer visit, possibly with committed progress before its result.
#[derive(Debug)]
pub struct SyncReport {
    /// Address selected for this attempt, absent when no candidate was eligible.
    pub peer: Option<SocketAddr>,
    /// Empty response, exhausted visit budget, or typed failure.
    pub result: Result<VisitEnd, SyncError>,
}

/// Why an otherwise successful visit ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitEnd {
    /// The peer supplied no further headers, not evidence of an honest fresh tip.
    EmptyResponse,
    /// Fairness budget exhausted; another call leases another available pool connection.
    RequestLimit,
    /// A due independent header check gets a turn at a completed exchange boundary.
    Maintenance,
}

impl HeaderSync {
    /// Take ownership of caller-initialized state and validate all budgets.
    pub fn new(store: ValidatedHeaderStore, config: SyncConfig) -> Result<Self, SyncError> {
        Self::new_with_mempool(store, config, false)
    }

    /// Opt into legacy transaction observations, historical fee samples and body coverage.
    ///
    /// This does not validate a mempool or persist transactions. The caller must
    /// drive `cover_mempool` between higher-priority work; RPC serving does so.
    pub fn new_with_mempool(
        store: ValidatedHeaderStore,
        config: SyncConfig,
        enabled: bool,
    ) -> Result<Self, SyncError> {
        if !config.validate(store.capacity()) {
            return Err(SyncError::Configuration);
        }
        let mut pool = PeerPool::new(&config.peers, config.peer_pool.clone())?;
        if enabled {
            pool.inbox = Some(crate::inventory_observer::InventoryInbox::new());
        }
        Ok(Self {
            pool,
            store,
            config,
            generation: 0,
            stopped: false,
            fee_policy: FeePolicy::default(),
            mempool: enabled.then(crate::mempool_observation::MempoolObservation::new),
            coverage_window: tokio::time::Instant::now(),
            coverage_bytes: 8 * 1024 * 1024,
            explorers: None,
            peer_start: None,
        })
    }

    /// Opt into independently paced untrusted HTTPS header candidates.
    ///
    /// This is distinct from trusted initial bootstrap. Each source retains at
    /// most 256 reverse headers (also capped by storage and candidate budgets).
    /// The caller must drive `explorer_step`; `GuardianRpc` does so automatically.
    pub fn with_explorer_headers(
        mut self,
        config: crate::explorer_header_config::ExplorerHeaderConfig,
    ) -> Result<Self, SyncError> {
        let explorers =
            crate::explorer_headers::ExplorerHeaders::new(config, self.config.candidate_headers)?;
        self.explorers = (!explorers.sources.is_empty()).then_some(explorers);
        Ok(self)
    }

    /// Next HTTP maintenance opportunity, never a wait inside a chain operation.
    pub fn explorer_deadline(&self) -> Option<tokio::time::Instant> {
        self.explorers
            .as_ref()
            .and_then(|sources| sources.deadline())
    }

    /// Perform at most one three-second HTTP request if due; never wait for pacing.
    ///
    /// Attempts and visit deadlines are reserved before awaiting, so cancellation
    /// retains cursor and charges the attempt. Remote failures only back off that
    /// source; actual local failures latch the same terminal state as P2P.
    /// Returns whether a maintenance opportunity was consumed, not tip freshness.
    pub async fn explorer_step(&mut self, now: u64) -> Result<bool, SyncError> {
        use crate::explorer_http::FetchError;
        if self.stopped {
            return Err(SyncError::Stopped);
        }
        let Some(index) = self
            .explorers
            .as_mut()
            .and_then(|sources| sources.reserve())
        else {
            return Ok(false);
        };
        // State stays in the owner across the await; dropping this future cannot
        // discard or refund a partially completed multi-visit snapshot.
        let sources = self.explorers.as_ref().expect("reserved source");
        let expected = sources.sources[index].expected;
        let (path, limit) = match expected {
            Some(hash) => (format!("block/{hash}/header"), 161),
            None => ("blocks/tip/hash".into(), 65),
        };
        let reply = sources.http.text(index, &path, limit).await;
        let text = match reply {
            Ok(text) => text,
            Err(error) => {
                let retry = match error {
                    FetchError::RateLimited(Some(delay)) => delay,
                    FetchError::RateLimited(None) => {
                        self.explorers.as_mut().expect("reserved source").sources[index]
                            .suspended = true;
                        tracing::warn!(
                            source = index,
                            "explorer suspended for this instance: excessive Retry-After"
                        );
                        std::time::Duration::ZERO
                    }
                    _ => std::time::Duration::ZERO,
                };
                self.explorers.as_mut().expect("reserved source").sources[index].fail(retry, false);
                tracing::debug!(source = index, ?error, "explorer header visit deferred");
                return Ok(true);
            }
        };
        let result = self.accept_explorer_reply(index, expected, &text, now);
        if let Err(error) = result {
            self.stopped |= error.is_terminal();
            self.explorers.as_mut().expect("reserved source").sources[index]
                .fail(std::time::Duration::ZERO, true);
            tracing::warn!(source = index, %error, "explorer candidate rejected");
            if self.stopped {
                return Err(error);
            }
        }
        Ok(true)
    }

    /// Decode hash-pinned evidence and enter the same atomic admission as P2P.
    fn accept_explorer_reply(
        &mut self,
        index: usize,
        expected: Option<BlockHash>,
        text: &str,
        now: u64,
    ) -> Result<(), SyncError> {
        use bitcoin::hex::FromHex;
        let text = text.strip_suffix('\n').unwrap_or(text);
        let sources = self.explorers.as_mut().expect("reserved source");
        let source = &mut sources.sources[index];
        if let Some(hash) = expected {
            let bytes = <[u8; 80]>::from_hex(text)
                .map_err(|_| SyncError::Protocol("explorer header encoding"))?;
            let header: Header = bitcoin::consensus::deserialize(&bytes)
                .map_err(|_| SyncError::Protocol("explorer header encoding"))?;
            if header.block_hash() != hash {
                return Err(SyncError::Protocol("explorer header hash mismatch"));
            }
            check_declared_pow(&header, Target::MAX_ATTAINABLE_MAINNET)
                .map_err(HeaderError::from)?;
            source.expected = Some(header.prev_blockhash);
            source.reverse.push(header);
        } else {
            if text.len() != 64 {
                return Err(SyncError::Protocol("explorer tip hash length"));
            }
            source.expected = Some(
                text.parse()
                    .map_err(|_| SyncError::Protocol("explorer tip hash encoding"))?,
            );
        }
        // Intervening P2P selection can make an interior cached header canonical.
        // Scan retained state once rather than once per cached header.
        if source.generation != self.generation && !source.reverse.is_empty() {
            let hashes: std::collections::HashMap<_, _> = source
                .reverse
                .iter()
                .enumerate()
                .map(|(i, header)| (header.block_hash(), i))
                .collect();
            let (floor, tip) = self.store.bounds()?;
            for height in (floor..=tip).rev() {
                let hash = self.store.header(height)?.block_hash();
                if let Some(&position) = hashes.get(&hash) {
                    source.reverse.truncate(position);
                    source.expected = Some(hash);
                    break;
                }
            }
        }
        source.generation = self.generation;
        let parent_hash = source.expected.expect("decoded target");
        if let Some(mut parent) = self.store.height_for_hash(parent_hash)? {
            let mut candidate = std::mem::take(&mut source.reverse);
            source.expected = None;
            if !candidate.is_empty() {
                candidate.reverse();
                let generation = self.generation;
                self.select_candidate(&mut parent, &mut candidate, now)?;
                if generation != self.generation {
                    tracing::info!(
                        source = index,
                        height = parent,
                        generation = self.generation,
                        "higher-work explorer headers selected"
                    );
                }
            }
            self.explorers.as_mut().expect("reserved source").sources[index].complete();
        } else if source.reverse.len() >= sources.cap {
            return Err(SyncError::UnsupportedCandidate);
        }
        Ok(())
    }

    /// Whether observation and its additional network traffic were explicitly enabled.
    pub fn mempool_enabled(&self) -> bool {
        self.mempool.is_some()
    }

    /// Reconcile current full selected identities before consuming socket observations.
    fn refresh_mempool(&mut self) -> Result<(), StoreError> {
        if self.mempool.is_none() {
            return Ok(());
        }
        let result = (|| {
            let (floor, tip) = self.store.bounds()?;
            (floor.max(tip.saturating_sub(19))..=tip)
                .rev()
                .map(|height| Ok((height, self.store.header(height)?.block_hash())))
                .collect::<Result<Vec<_>, StoreError>>()
        })();
        let selected = result.inspect_err(|_| self.stopped = true)?;
        let cache = self.mempool.as_mut().expect("enabled");
        cache.reconcile(selected);
        let now = tokio::time::Instant::now();
        cache.expire(now);
        let mut inbox = self
            .pool
            .inbox
            .as_ref()
            .expect("enabled")
            .lock()
            .expect("no panicking inbox owner");
        for (txid, source) in inbox.queue.drain(..) {
            cache.observe(txid, source, now);
        }
        Ok(())
    }

    /// Return an approximate snapshot, or None when disabled; reads never refresh TTL.
    pub fn mempool_snapshot(&mut self) -> Result<Option<Vec<bitcoin::Txid>>, StoreError> {
        self.refresh_mempool()?;
        Ok(self.mempool.as_ref().map(|cache| cache.snapshot()))
    }

    /// Return historical, partial, unvalidated fee observations, or None when disabled.
    ///
    /// Enabled by `new_with_mempool`; this read never fetches parents or blocks,
    /// redraws samples, changes coverage priority, or changes `fee_policy`.
    pub fn fee_observations(
        &mut self,
    ) -> Result<Option<crate::fee_observation_status::FeeObservationStatus>, StoreError> {
        self.refresh_mempool()?;
        Ok(self.mempool.as_ref().map(|cache| cache.fee_status()))
    }

    /// Report native approximate-view coverage and bounded-state losses.
    pub fn mempool_status(
        &mut self,
    ) -> Result<Option<crate::mempool_observation::MempoolObservationStatus>, StoreError> {
        self.refresh_mempool()?;
        Ok(self.mempool.as_ref().map(|cache| {
            let inbox = self
                .pool
                .inbox
                .as_ref()
                .expect("enabled")
                .lock()
                .expect("no panicking inbox owner");
            let mut status = cache.status();
            status.inventory_dropped = inbox.dropped;
            status
        }))
    }

    /// Attempt one newest-missing selected body, with no retained body cache.
    ///
    /// Reserve 4 MiB from an 8 MiB/minute allowance before any await, including
    /// failed/canceled attempts. Inventory has its separate aggregate allowance.
    /// Missing/invalid remote bodies are transient; local storage failures stop
    /// the owner under the normal policy. Dropping this future closes its lease.
    pub async fn cover_mempool(&mut self) -> Result<(), BlockDownloadError> {
        self.cover_mempool_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .await
    }

    /// Reserve only when the owner's actual remaining slice permits useful work.
    pub(crate) async fn cover_mempool_until(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<(), BlockDownloadError> {
        self.refresh_mempool()?;
        let Some((height, hash)) = self.mempool.as_ref().and_then(|cache| cache.missing()) else {
            return Ok(());
        };
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(std::time::Duration::from_secs(5));
        if remaining < std::time::Duration::from_millis(250) {
            return Ok(());
        }
        if self.coverage_window.elapsed() >= std::time::Duration::from_secs(60) {
            self.coverage_window = tokio::time::Instant::now();
            self.coverage_bytes = 8 * 1024 * 1024;
        }
        const RESERVATION: usize = 4 * 1024 * 1024;
        if self.coverage_bytes < RESERVATION {
            return Ok(());
        }
        self.coverage_bytes -= RESERVATION;
        let result = self
            .fetch_block(
                height,
                hash,
                BlockDownloadConfig {
                    max_attempts: 1,
                    total_timeout: remaining,
                    visit_timeout: remaining,
                    max_received_bytes: RESERVATION,
                    decode: crate::block_decode::BlockDecodeLimits {
                        max_payload_bytes: 4_000_000,
                        max_elements: 1_000_000,
                    },
                },
            )
            .await;
        tracing::debug!(event = "mempool_coverage", height, %hash,
            outcome = if result.is_ok() { "covered" } else { "failed" },
            "approximate observation coverage attempt");
        match result {
            Ok(_) => Ok(()),
            Err(error) if self.stopped => Err(error),
            Err(_) => Ok(()),
        }
    }

    /// Set explicit fee information; positive fixed rates are enforced by the type.
    ///
    /// The default is unavailable. Policy is caller configuration, not persisted
    /// chain state: the caller must supply it again after reopening.
    pub fn with_fee_policy(mut self, policy: FeePolicy) -> Self {
        self.fee_policy = policy;
        self
    }

    /// Return explicit fee policy without contacting a peer or estimating fees.
    pub fn fee_policy(&self) -> FeePolicy {
        self.fee_policy
    }

    /// Return selected tip height plus one, without protecting the returned state.
    pub fn block_count(&self) -> Result<u64, StoreError> {
        self.status()?.block_count()
    }

    /// Return pinned height-ONE identity, even after that header has been pruned.
    pub fn chain_id(&self) -> BlockHash {
        self.store.anchor().height_one_hash
    }

    /// Resolve a retained selected height without fetching or protecting history.
    ///
    /// Old heights return `OutsideRetainedHistory`; future heights return
    /// `BeyondTip`. This never downloads historical headers.
    pub fn block_hash(&self, height: u64) -> Result<BlockHash, StoreError> {
        Ok(self.store.header(height)?.block_hash())
    }

    /// Return no progress estimate: no credible network-tip denominator is known.
    pub fn sync_progress(&self) -> Option<f64> {
        None
    }

    /// Return current selected state, also after cancellation or a failed visit.
    pub fn status(&self) -> Result<SyncStatus, StoreError> {
        let context = self.store.tip_context()?;
        Ok(SyncStatus {
            fee_policy: self.fee_policy,
            height: context.height(),
            hash: context.tip().block_hash(),
            generation: self.generation,
            stopped: self.stopped,
        })
    }

    /// Borrow retained selected headers without opening a network transaction.
    pub fn store(&self) -> &ValidatedHeaderStore {
        &self.store
    }

    /// Protect consumed history between visits before promising consumer finality.
    pub fn protect_through(&mut self, height: u64, hash: BlockHash) -> Result<(), StoreError> {
        let result = self.store.protect_through(height, hash);
        if result.as_ref().is_err_and(|error| {
            !matches!(
                error,
                StoreError::InvalidUpdate(_)
                    | StoreError::OutsideRetainedHistory
                    | StoreError::BeyondTip
            )
        }) {
            self.stopped = true;
        }
        result
    }

    /// Return storage ownership and abort owned pool tasks; use `shutdown` to join first.
    pub fn into_store(self) -> ValidatedHeaderStore {
        self.store
    }

    /// Close and join all owned idle connections; OS DNS shutdown has documented limits.
    pub async fn shutdown(&mut self) {
        self.pool.shutdown().await;
    }

    /// Return live idle sockets, known candidates and whether the last cache write failed.
    pub fn peer_counts(&self) -> (usize, usize, bool) {
        self.pool.counts()
    }

    /// Submit a caller-owned transaction over bounded sequential mainnet connections.
    ///
    /// Admission bounds bytes and weight before outbound allocation; it does not
    /// check scripts, spends, fees, standardness or consensus validity. This exclusive
    /// borrow shares the network owner and peer rotation with sync and block retrieval.
    /// No transaction queue, cache, retry task or chain database write is created.
    ///
    /// An `Ok` report can contain zero submissions: inspect `report.submitted()`.
    /// Only a completed full frame write counts, not peer receipt, mempool acceptance,
    /// propagation or confirmation. All selected peers are tried even after success.
    /// A bounded post-write ping/pong observation reduces premature connection close;
    /// it is not an acceptance acknowledgement and cannot undo a completed write.
    /// Dropping the future closes the current socket and loses its report, including
    /// earlier successes. Cancellation or a failed started write can occur after the
    /// peer received the transaction: the outcome is indeterminate. Explicit caller
    /// retry/rebroadcast is allowed; there is no deduplication or exactly-once promise.
    /// Bounded synchronous serialization and hashing cannot be interrupted.
    pub async fn submit_transaction(
        &mut self,
        transaction: &Transaction,
        config: TransactionSubmissionConfig,
    ) -> Result<TransactionSubmissionReport, TransactionSubmissionError> {
        if self.stopped {
            return Err(TransactionSubmissionError::Stopped);
        }
        submit(transaction, &mut self.pool, self.config.retry_delay, config).await
    }

    /// Retrieve a complete witness-bearing block matching a retained selected hash.
    ///
    /// The exclusive borrow prevents header selection, pruning, protection changes,
    /// and concurrent downloads until this call returns or is dropped. No database
    /// transaction crosses an await. Thus the hash remains canonical at delivery,
    /// without a stale-selection race. It can change on a later `step`: this method
    /// does not promise finality or automatically raise the consumer protection floor.
    /// Call `protect_through` before promising finality to consumers.
    ///
    /// The compiler prevents a concurrent reorganization or second download:
    /// ```compile_fail
    /// # use fedimint_light_node::{header_sync::HeaderSync, block_download_config::BlockDownloadConfig};
    /// # async fn example(node: &mut HeaderSync, config: BlockDownloadConfig) {
    /// let tip = node.status().unwrap();
    /// let pending = node.fetch_block(tip.height, tip.hash, config);
    /// node.step(2_000_000_000).await;
    /// pending.await.unwrap();
    /// # }
    /// ```
    ///
    /// One foreground payload and decoder are shared globally per service, not per
    /// peer. There is no body cache, pipeline or queue. Dropping the
    /// future immediately releases network/buffer ownership; synchronous bounded
    /// decode/checking between awaits cannot be interrupted. Returned blocks are
    /// caller-owned and excluded from the library budget. No store writes occur.
    ///
    /// Each call uses a bounded number of pool leases with pacing and absolute
    /// visit/total deadlines. Only NODE_NETWORK + NODE_WITNESS peers qualify:
    /// limited-only peers promise their own recent 288 blocks, and neither our tip
    /// nor their advertised height proves that our request is in that range.
    /// Unavailability is not evidence of stale headers or invalid chain selection.
    ///
    /// Mainnet witness activation follows retained height, not untrusted body data.
    /// Declared PoW is checked even for the caller-trusted anchor; descendants were
    /// already contextually admitted. Body checks are mechanical, not full consensus.
    pub async fn fetch_block(
        &mut self,
        height: u64,
        hash: BlockHash,
        config: BlockDownloadConfig,
    ) -> Result<Block, BlockDownloadError> {
        if !config.validate() {
            return Err(BlockDownloadError::Configuration);
        }
        if self.stopped {
            return Err(BlockDownloadError::Stopped);
        }
        let header = match self.store.header(height) {
            Ok(header) => header,
            Err(StoreError::OutsideRetainedHistory) => {
                return Err(BlockDownloadError::OutsideRetainedHistory);
            }
            Err(StoreError::BeyondTip) => return Err(BlockDownloadError::BeyondTip),
            Err(error) => {
                self.stopped = true;
                return Err(error.into());
            }
        };
        if header.block_hash() != hash {
            return Err(BlockDownloadError::NotCanonical);
        }
        if let Err(error) = check_declared_pow(&header, Target::MAX_ATTAINABLE_MAINNET) {
            self.stopped = true;
            return Err(error.into());
        }
        let tip_height = self
            .store
            .bounds()
            .map_err(|error| {
                self.stopped = true;
                BlockDownloadError::Store(error)
            })?
            .1;
        let block = download(
            &mut self.pool,
            self.config.retry_delay,
            tip_height,
            height,
            hash,
            config,
        )
        .await?;
        self.refresh_mempool()?;
        if let Some(cache) = &mut self.mempool {
            cache.ingest(height, &block);
        }
        Ok(block)
    }

    /// Visit one peer with absolute exchange deadlines and bounded work.
    ///
    /// The caller repeats this method to continue synchronization. Every attempt
    /// incurs retry pacing, including empty responses, invalid peers and cancellation.
    /// A visit uses at most one handshake plus `requests_per_visit` requests,
    /// each with at most 32 frames and 2 MB received bytes. A peer cannot extend
    /// deadlines using pings, fragmented frames or incremental progress.
    /// `now` is a trusted caller wall-clock snapshot for contextual header checks.
    /// Future-time rejection discards the connection but preserves retry eligibility:
    /// it grants no useful-service credit and retains monotonic backoff. Correcting
    /// `now` never revalidates already-selected history or proves tip freshness.
    /// Cancel by dropping this future (for example with `tokio::select!`); inspect
    /// `status` afterward because a completed atomic batch may have made progress.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn step(&mut self, now: u64) -> SyncReport {
        self.step_inner(now, false).await
    }

    /// Caller-driven synchronization with explorer checks during pacing and at
    /// completed exchange boundaries, preserving configured P2P exchange deadlines.
    ///
    /// Unlike RPC's short slices, this can wait the configured retry interval,
    /// driving due HTTP maintenance while waiting. A due check can end a peer visit
    /// early, dropping its unselected candidate without changing selected history.
    pub async fn step_with_explorers(&mut self, now: u64) -> SyncReport {
        self.step_inner(now, true).await
    }

    /// Wait against an original absolute pacing deadline, never restart it when
    /// an explorer operation consumes part of the interval.
    async fn pace_until(
        &mut self,
        until: tokio::time::Instant,
        now: u64,
        maintain: bool,
    ) -> Result<(), SyncError> {
        while tokio::time::Instant::now() < until {
            let due = maintain.then(|| self.explorer_deadline()).flatten();
            if let Some(due) = due {
                tokio::time::sleep_until(until.min(due)).await;
                if tokio::time::Instant::now() < until {
                    self.explorer_step(now).await?;
                }
            } else {
                tokio::time::sleep_until(until).await;
            }
        }
        Ok(())
    }

    async fn step_inner(&mut self, now: u64, maintain: bool) -> SyncReport {
        let started = std::time::Instant::now();
        if self.stopped {
            return SyncReport {
                peer: None,
                result: Err(SyncError::Stopped),
            };
        }
        if maintain && let Err(error) = self.explorer_step(now).await {
            return SyncReport {
                peer: None,
                result: Err(error),
            };
        }
        let start = *self
            .peer_start
            .get_or_insert_with(|| tokio::time::Instant::now() + self.config.retry_delay);
        if let Err(error) = self.pace_until(start, now, maintain).await {
            return SyncReport {
                peer: None,
                result: Err(error),
            };
        }
        self.peer_start = None;
        let prepared = self
            .pool
            .prepare_acquire(self.config.retry_delay, self.config.handshake_timeout, true)
            .await;
        if let Err(error) = self.pace_until(prepared, now, maintain).await {
            return SyncReport {
                peer: None,
                result: Err(error),
            };
        }
        let tip = match self.store.bounds() {
            Ok((_, tip)) => tip,
            Err(error) => {
                tracing::error!(%error, "header store failed; stopping synchronization");
                self.stopped = true;
                return SyncReport {
                    peer: None,
                    result: Err(error.into()),
                };
            }
        };
        let mut sent = usize::MAX;
        let mut connection = match self
            .pool
            .acquire_prepared(
                tip,
                self.config.retry_delay,
                self.config.handshake_timeout,
                &mut 2_000_000,
                &mut sent,
            )
            .await
        {
            Ok(connection) => connection,
            Err((peer, error)) => {
                tracing::debug!(?peer, %error, "peer acquisition failed");
                return SyncReport {
                    peer,
                    result: Err(error),
                };
            }
        };
        let peer = Some(connection.addr);
        if maintain && let Err(error) = self.explorer_step(now).await {
            return SyncReport {
                peer,
                result: Err(error),
            };
        }
        let mut useful = false;
        let result = self
            .visit(&mut connection, now, &mut useful, maintain)
            .await;
        if result.as_ref().is_err_and(SyncError::is_terminal) {
            self.stopped = true;
            tracing::error!(?peer, error = ?result, "terminal header visit failure");
        }
        tracing::debug!(?peer, outcome = ?result, elapsed_ms = started.elapsed().as_millis(),
            "header visit completed");
        if result.is_ok() {
            self.pool.release(connection, useful);
        } else if matches!(result, Err(SyncError::Header(HeaderError::FutureTime))) {
            // A caller clock behind the peer's timestamps must not exhaust a
            // discovered candidate's failure allowance. Still discard the stream.
            self.pool.rejected_time(&connection);
        }
        SyncReport { peer, result }
    }

    async fn visit(
        &mut self,
        stream: &mut PeerConnection,
        now: u64,
        useful: &mut bool,
        maintain: bool,
    ) -> Result<VisitEnd, SyncError> {
        let mut candidate: Vec<Header> = Vec::with_capacity(self.config.candidate_headers);
        let mut parent = 0;
        for request_index in 0..self.config.requests_per_visit {
            let locator = if let Some(last) = candidate.last() {
                vec![(parent + candidate.len() as u64, last.block_hash())]
            } else {
                // Start one header behind when possible: checked canonical overlap
                // demonstrates useful service even when no new block was mined.
                let mut locator = self.locator()?;
                if request_index == 0 && self.config.peer_pool.discovery && locator.len() > 1 {
                    locator.swap(0, 1);
                }
                locator
            };
            let headers = timeout(self.config.request_timeout, request(stream, &locator))
                .await
                .map_err(|_| SyncError::Timeout)??;
            if headers.is_empty() {
                return Ok(VisitEnd::EmptyResponse);
            }
            if candidate.is_empty() {
                parent = locator
                    .iter()
                    .find(|(_, hash)| *hash == headers[0].prev_blockhash)
                    .map(|(height, _)| *height)
                    .ok_or(SyncError::UnsupportedCandidate)?;
            } else if headers[0].prev_blockhash != candidate.last().expect("nonempty").block_hash()
            {
                return Err(SyncError::Protocol("candidate continuation linkage"));
            }
            for header in headers {
                // Peers can return already-selected headers from an older locator.
                // Do not spend the fork budget on identical canonical ancestry.
                if candidate.is_empty()
                    && parent < self.store.bounds()?.1
                    && self.store.header(parent + 1)? == header
                {
                    *useful = true;
                    parent += 1;
                    continue;
                }
                candidate.push(header);
                if candidate.len() == self.config.candidate_headers {
                    self.select_candidate(&mut parent, &mut candidate, now)?;
                    *useful |= candidate.is_empty();
                    if !candidate.is_empty() {
                        return Err(SyncError::UnsupportedCandidate);
                    }
                }
            }
            if !candidate.is_empty() {
                self.select_candidate(&mut parent, &mut candidate, now)?;
                *useful |= candidate.is_empty();
            }
            if maintain
                && self
                    .explorer_deadline()
                    .is_some_and(|due| due <= tokio::time::Instant::now())
            {
                return Ok(VisitEnd::Maintenance);
            }
        }
        Ok(VisitEnd::RequestLimit)
    }

    fn select_candidate(
        &mut self,
        parent: &mut u64,
        candidate: &mut Vec<Header>,
        now: u64,
    ) -> Result<(), SyncError> {
        // Reserve the counter before changing persistent state.
        let next = self.generation.checked_add(1).ok_or(SyncError::Stopped)?;
        match self.store.select_suffix(*parent, candidate, now) {
            Ok(()) => {
                *parent += candidate.len() as u64;
                candidate.clear();
                self.generation = next;
                Ok(())
            }
            Err(HeaderError::InsufficientWork) => Ok(()),
            Err(HeaderError::RecoveryRequired) => Err(SyncError::UnsupportedCandidate),
            Err(error) => Err(error.into()),
        }
    }

    fn locator(&self) -> Result<Vec<(u64, BlockHash)>, StoreError> {
        let (first, tip) = self.store.bounds()?;
        let mut result = Vec::with_capacity(80);
        let mut height = tip;
        let mut stride = 1u64;
        loop {
            result.push((height, self.store.header(height)?.block_hash()));
            if height == first {
                break;
            }
            if result.len() > 10 {
                stride = stride.saturating_mul(2);
            }
            height = height.saturating_sub(stride).max(first);
        }
        Ok(result)
    }
}

async fn request(
    stream: &mut PeerConnection,
    locator: &[(u64, BlockHash)],
) -> Result<Vec<Header>, SyncError> {
    send(
        &mut stream.stream,
        NetworkMessage::GetHeaders(GetHeadersMessage::new(
            locator.iter().map(|(_, hash)| *hash).collect(),
            BlockHash::all_zeros(),
        )),
    )
    .await?;
    let mut bytes = 2_000_000;
    for _ in 0..32 {
        match stream.receive(&mut bytes, None).await? {
            Message::Headers(headers) => return Ok(headers),
            Message::Ping(nonce) => send(&mut stream.stream, NetworkMessage::Pong(nonce)).await?,
            Message::Ignore | Message::Pong(_) => {}
            _ => return Err(SyncError::Protocol("unexpected request message")),
        }
    }
    Err(SyncError::Protocol("request message budget"))
}

#[cfg(test)]
#[path = "header_sync_observation_tests.rs"]
mod observation_tests;
