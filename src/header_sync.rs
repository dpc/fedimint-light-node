use std::net::{Ipv4Addr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};

use bitcoin::hashes::Hash;
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::GetHeadersMessage;
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::{BlockHash, block::Header};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

use crate::header_error::HeaderError;
use crate::header_wire::{Message, receive, send};
use crate::store_error::StoreError;
use crate::sync_config::SyncConfig;
use crate::sync_error::SyncError;
use crate::validated_header_store::ValidatedHeaderStore;

/// Caller-driven mainnet v1 header synchronization from an initialized trusted store.
///
/// Each `step` visits one configured peer; repeated calls provide paced, fair
/// round-robin synchronization. There is no background task, listener, discovery,
/// runtime creation, body request, or global configuration. A caller must supply
/// a Tokio runtime with I/O and time enabled. One global socket and candidate
/// avoid multiplicative per-peer budgets; sequential peers trade throughput and
/// availability for a small auditable lifecycle, not production eclipse resistance.
///
/// Dropping the `step` future closes its socket and drops any unselected candidate.
/// No work survives cancellation. Already committed progress remains observable
/// through `status`, including when a later request fails or gets cancelled.
/// Synchronous bounded validation and redb transactions run between awaits:
/// they cannot be interrupted and may block the executor. Use a caller-owned
/// dedicated runtime thread if that latency is unacceptable.
pub struct HeaderSync {
    /// Exclusive validated state; never held in a transaction across an await.
    store: ValidatedHeaderStore,
    /// Validated global resource and time budgets.
    config: SyncConfig,
    /// Next configured peer; advanced before every attempt, even cancellation.
    next_peer: usize,
    /// Monotonic in-memory selection generation, not a persisted event log.
    generation: u64,
    /// Terminal local/recovery failures require explicit caller intervention.
    stopped: bool,
}

/// Selected state, not a freshness claim or a consensus-valid chain guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncStatus {
    /// Current selected height.
    pub height: u64,
    /// Current selected hash.
    pub hash: BlockHash,
    /// Number of canonical updates in this service instance; resets on restart.
    pub generation: u64,
    /// A terminal error has stopped further visits.
    pub stopped: bool,
}

/// One bounded peer visit, possibly with committed progress before its result.
#[derive(Debug)]
pub struct SyncReport {
    /// Configured address selected for this attempt.
    pub peer: SocketAddr,
    /// Empty response, exhausted visit budget, or typed failure.
    pub result: Result<VisitEnd, SyncError>,
}

/// Why an otherwise successful visit ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitEnd {
    /// The peer supplied no further headers, not evidence of an honest fresh tip.
    EmptyResponse,
    /// Fairness budget exhausted; another call visits the next configured peer.
    RequestLimit,
}

impl HeaderSync {
    /// Take ownership of caller-initialized state and validate all budgets.
    pub fn new(store: ValidatedHeaderStore, config: SyncConfig) -> Result<Self, SyncError> {
        if !config.validate(store.capacity()) {
            return Err(SyncError::Configuration);
        }
        Ok(Self {
            store,
            config,
            next_peer: 0,
            generation: 0,
            stopped: false,
        })
    }

    /// Return current selected state, also after cancellation or a failed visit.
    pub fn status(&self) -> Result<SyncStatus, StoreError> {
        let context = self.store.tip_context()?;
        Ok(SyncStatus {
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

    /// Return storage ownership; no network work or background tasks remain.
    pub fn into_store(self) -> ValidatedHeaderStore {
        self.store
    }

    /// Visit one peer with absolute exchange deadlines and bounded work.
    ///
    /// The caller repeats this method to continue synchronization. Every attempt
    /// incurs retry pacing, including empty responses, invalid peers and cancellation.
    /// A visit uses at most one handshake plus `requests_per_visit` requests,
    /// each with at most 32 frames and 2 MB received bytes. A peer cannot extend
    /// deadlines using pings, fragmented frames or incremental progress.
    /// `now` is a trusted caller wall-clock snapshot for contextual header checks.
    /// Cancel by dropping this future (for example with `tokio::select!`); inspect
    /// `status` afterward because a completed atomic batch may have made progress.
    pub async fn step(&mut self, now: u64) -> SyncReport {
        let peer = self.config.peers[self.next_peer];
        if self.stopped {
            return SyncReport {
                peer,
                result: Err(SyncError::Stopped),
            };
        }
        self.next_peer = (self.next_peer + 1) % self.config.peers.len();
        sleep(self.config.retry_delay).await;
        let result = self.visit(peer, now).await;
        if result.as_ref().is_err_and(SyncError::is_terminal) {
            self.stopped = true;
        }
        SyncReport { peer, result }
    }

    async fn visit(&mut self, peer: SocketAddr, now: u64) -> Result<VisitEnd, SyncError> {
        let mut stream = timeout(self.config.handshake_timeout, self.handshake(peer))
            .await
            .map_err(|_| SyncError::Timeout)??;
        let mut candidate: Vec<Header> = Vec::with_capacity(self.config.candidate_headers);
        let mut parent = 0;
        for _ in 0..self.config.requests_per_visit {
            let locator = if let Some(last) = candidate.last() {
                vec![(parent + candidate.len() as u64, last.block_hash())]
            } else {
                self.locator()?
            };
            let headers = timeout(self.config.request_timeout, request(&mut stream, &locator))
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
                    parent += 1;
                    continue;
                }
                candidate.push(header);
                if candidate.len() == self.config.candidate_headers {
                    self.select_candidate(&mut parent, &mut candidate, now)?;
                    if !candidate.is_empty() {
                        return Err(SyncError::UnsupportedCandidate);
                    }
                }
            }
            if !candidate.is_empty() {
                self.select_candidate(&mut parent, &mut candidate, now)?;
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

    async fn handshake(&self, peer: SocketAddr) -> Result<TcpStream, SyncError> {
        let mut stream = TcpStream::connect(peer).await?;
        let mut version = VersionMessage::new(
            ServiceFlags::NONE,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            Address::new(&peer, ServiceFlags::NONE),
            Address::new(
                &SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
                ServiceFlags::NONE,
            ),
            0, // No listener: no self-connection detection is needed.
            "/fedimint-light-node:0.1/".into(),
            self.store.bounds()?.1.min(i32::MAX as u64) as i32,
        );
        version.relay = false;
        send(&mut stream, NetworkMessage::Version(version)).await?;
        let mut got_version = false;
        let mut bytes = 2_000_000;
        for _ in 0..32 {
            match receive(&mut stream, &mut bytes).await? {
                Message::Version(version) if !got_version => {
                    if version.version < 70001
                        || !(version.services.has(ServiceFlags::NETWORK)
                            || version.services.has(ServiceFlags::NETWORK_LIMITED))
                    {
                        return Err(SyncError::Protocol("unsupported peer version or services"));
                    }
                    got_version = true;
                    send(&mut stream, NetworkMessage::Verack).await?;
                }
                Message::Verack if got_version => return Ok(stream),
                Message::Ping(nonce) => send(&mut stream, NetworkMessage::Pong(nonce)).await?,
                Message::Ignore => {}
                _ => return Err(SyncError::Protocol("unexpected handshake message")),
            }
        }
        Err(SyncError::Protocol("handshake message budget"))
    }
}

async fn request(
    stream: &mut TcpStream,
    locator: &[(u64, BlockHash)],
) -> Result<Vec<Header>, SyncError> {
    send(
        stream,
        NetworkMessage::GetHeaders(GetHeadersMessage::new(
            locator.iter().map(|(_, hash)| *hash).collect(),
            BlockHash::all_zeros(),
        )),
    )
    .await?;
    let mut bytes = 2_000_000;
    for _ in 0..32 {
        match receive(stream, &mut bytes).await? {
            Message::Headers(headers) => return Ok(headers),
            Message::Ping(nonce) => send(stream, NetworkMessage::Pong(nonce)).await?,
            Message::Ignore => {}
            _ => return Err(SyncError::Protocol("unexpected request message")),
        }
    }
    Err(SyncError::Protocol("request message budget"))
}
