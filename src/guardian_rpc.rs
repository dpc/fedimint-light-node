//! Narrow guardian transport, not a general Bitcoin Core or consensus service.
//!
//! One caller-owned future serializes store/pool access. At most eight HTTP
//! connections, four cheap queued reads and one queued network operation exist;
//! no task is detached. Cheap reads take priority between bounded network visits. A request
//! has a fourteen-second transport deadline (including reads and writes), shorter
//! than the pinned guardian client's fifteen-second timeout. Queue cancellation
//! cancels leased network operations but cannot undo a completed transaction write.
//! A four-second sync slice becomes due one second after the preceding visit
//! (five seconds with observation enabled, allowing bounded body coverage);
//! an active bounded RPC may defer it, but queued requests cannot starve it.
//! Synchronous database/validation work remains non-preemptible.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use bitcoin::hashes::{Hash, sha256};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};

use crate::guardian_rpc_config::{GuardianRpcConfig, RpcAuth};
use crate::header_sync::HeaderSync;
use crate::rpc_method::{RpcError, RpcRequest, dispatch, fee_value};

/// Fields deliberately synthesized only when `fake_bitcoind` is enabled.
pub const SYNTHETIC_STATUS_WARNING: &str = "fake-bitcoind: synthetic difficulty=1, mediantime=0, verificationprogress=1, initialblockdownload=false, chainwork=zero, size_on_disk=0, pruned=true, version=190000; NOT full validation, freshness, absolute work, Core pruning or mempool acceptance";

/// Observation trades polling latency for a usable body slice, never changes due priority.
fn sync_interval(mempool: bool) -> Duration {
    Duration::from_secs(if mempool { 5 } else { 1 })
}

/// Bounded loopback listener and exclusive foreground chain owner.
pub struct GuardianRpc {
    /// Bound before serving; no implicit bootstrap or state replacement.
    listener: TcpListener,
    /// Exclusive pool and persistent store owner.
    sync: HeaderSync,
    /// Hash of the expected authorization header, compared in constant time.
    authorization: Option<[u8; 32]>,
    /// Explicit synthetic schema permission.
    fake: bool,
    /// Explicit submission-only acknowledgement.
    submission: bool,
}

/// One bounded request, discarded if its waiting transport has gone away.
pub(crate) struct Pending {
    /// Parsed, bounded positional request.
    pub request: RpcRequest,
    /// Closing this receiver cancels in-flight asynchronous owner work.
    pub reply: oneshot::Sender<Result<serde_json::Value, RpcError>>,
}

impl GuardianRpc {
    /// Validate policy, local readiness and fee round-trip before binding loopback.
    pub async fn bind(config: GuardianRpcConfig, sync: HeaderSync) -> io::Result<Self> {
        if !config.bind.ip().is_loopback() {
            return Err(io::Error::other("RPC requires a numeric loopback address"));
        }
        let authorization = match config.auth {
            RpcAuth::NoAuth if config.allow_submission => {
                return Err(io::Error::other(
                    "Unauthenticated RPC forbids transaction submission",
                ));
            }
            RpcAuth::NoAuth => None,
            RpcAuth::Basic { username, password } => {
                if username.is_empty()
                    || password.is_empty()
                    || username.contains(':')
                    || username.len() + password.len() > 512
                    || username
                        .chars()
                        .chain(password.chars())
                        .any(char::is_control)
                {
                    return Err(io::Error::other(
                        "RPC requires explicit bounded credentials",
                    ));
                }
                Some(
                    sha256::Hash::hash(
                        format!(
                            "Basic {}",
                            base64::engine::general_purpose::STANDARD
                                .encode(format!("{username}:{password}"))
                        )
                        .as_bytes(),
                    )
                    .to_byte_array(),
                )
            }
        };
        let status = sync.status().map_err(io::Error::other)?;
        if status.stopped {
            return Err(io::Error::other("RPC owner requires explicit recovery"));
        }
        fee_value(sync.fee_policy()).map_err(|e| io::Error::other(e.message))?;
        Ok(Self {
            listener: TcpListener::bind(config.bind).await?,
            sync,
            authorization,
            fake: config.fake_bitcoind,
            submission: config.allow_submission,
        })
    }

    /// Return the bound address, including the assigned port when zero was requested.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serve until caller cancellation or a terminal local failure; join owned tasks.
    ///
    /// The observer sees native selected state and visit diagnostics, never synthetic
    /// readiness. No signal handler, runtime, logging sink or permanent task is installed.
    pub async fn run(
        mut self,
        shutdown: impl Future<Output = ()>,
        mut on_sync: impl FnMut(&HeaderSync, &str),
    ) -> io::Result<()> {
        let (send, mut receive) = mpsc::channel::<Pending>(4);
        let (slow_send, mut slow_receive) = mpsc::channel::<Pending>(1);
        let mut connections = JoinSet::new();
        let authorization = self.authorization;
        let listener = &self.listener;
        let network = async {
            loop {
                tokio::select! {
                    accepted = listener.accept(), if connections.len() < 8 => {
                        let (stream, _) = accepted?;
                        let send = send.clone();
                        let slow_send = slow_send.clone();
                        connections.spawn(async move {
                            #[cfg(all(test, target_os = "linux"))]
                            let _accepted = crate::readiness_observer::enter(
                                crate::readiness_observer::Counter::Accepted,
                            );
                            let _ = timeout(Duration::from_secs(14),
                                crate::rpc_http::serve(stream, authorization, send, slow_send)).await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), io::Error>(())
        };
        let owner = async {
            let mut next_sync = Instant::now();
            let mut next_coverage = Instant::now();
            let mut next_mempool_summary = Instant::now();
            let mut completed = 0_u64;
            let mut canceled = 0_u64;
            let mut last_code = 0_i32;
            let mut last_rpc_unix = None;
            let mut last_visit_ok_unix = None;
            let mut last_selection_unix = None;
            let mut generation = self.sync.status().map_err(io::Error::other)?.generation;
            loop {
                // Check the clock before dequeuing: a full queue cannot starve sync.
                if Instant::now() >= next_sync {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(io::Error::other)?
                        .as_secs();
                    let report = timeout(Duration::from_secs(4), self.sync.step(now)).await;
                    if report.as_ref().is_ok_and(|report| report.result.is_ok()) {
                        last_visit_ok_unix = Some(now);
                    }
                    let status = self.sync.status().map_err(io::Error::other)?;
                    if status.generation != generation {
                        last_selection_unix = Some(now);
                        generation = status.generation;
                    }
                    let diagnostic = match report {
                        Ok(report) => format!("{:?}", report.result),
                        Err(_) => "visit deadline (progress, if committed, is retained)".into(),
                    };
                    let diagnostic = format!(
                        "observed_unix={now} stopped={} cheap_queued={} network_queued={} rpc_completed={completed} rpc_canceled={canceled} last_rpc_code={last_code} last_rpc_unix={last_rpc_unix:?} last_visit_ok_unix={last_visit_ok_unix:?} last_selection_unix={last_selection_unix:?} visit={diagnostic}",
                        status.stopped,
                        receive.len(),
                        slow_receive.len()
                    );
                    let mempool = self.sync.mempool_status().map_err(io::Error::other)?;
                    if let Some(status) = mempool
                        && Instant::now() >= next_mempool_summary
                    {
                        tracing::info!(
                            event = "mempool_summary",
                            ?status,
                            "approximate, incomplete and unvalidated observation"
                        );
                        next_mempool_summary = Instant::now() + Duration::from_secs(60);
                    }
                    let diagnostic = format!("{diagnostic} approximate_mempool={mempool:?}");
                    on_sync(&self.sync, &diagnostic);
                    if self.sync.status().map_err(io::Error::other)?.stopped {
                        return Err(io::Error::other("sync stopped; explicit recovery required"));
                    }
                    next_sync = Instant::now() + sync_interval(self.sync.mempool_enabled());
                }
                let request = tokio::select! {
                    biased;
                    request = receive.recv() => request,
                    _ = tokio::time::sleep_until(next_sync) => continue,
                    request = slow_receive.recv() => request,
                    result = async {
                        tokio::time::sleep_until(next_coverage).await;
                        // Finish slightly before the due header when possible. Synchronous
                        // decoding remains non-preemptible; no whole-five-second promise.
                        self.sync.cover_mempool_until(next_sync - Duration::from_millis(25)).await
                    }, if self.sync.mempool_enabled() => {
                        result.map_err(io::Error::other)?;
                        // One completed maintenance slice per header interval. Neither
                        // failed coverage nor RPC traffic moves the header deadline.
                        next_coverage = next_sync;
                        continue;
                    },
                };
                let Some(mut pending) = request else {
                    return Ok(());
                };
                #[cfg(all(test, target_os = "linux"))]
                crate::readiness_observer::queue(pending.request.is_network(), false);
                if pending.reply.is_closed() {
                    canceled = canceled.saturating_add(1);
                    continue;
                }
                let started = Instant::now();
                let result = tokio::select! {
                    biased;
                    _ = pending.reply.closed() => {
                        canceled = canceled.saturating_add(1);
                        tracing::debug!(method = pending.request.log_method(), outcome = "canceled",
                            "RPC request canceled");
                        continue;
                    },
                    result = timeout(Duration::from_secs(6),
                        dispatch(&mut self.sync, &pending.request, self.fake, self.submission)) =>
                        result.unwrap_or_else(|_| Err(RpcError::unavailable())),
                };
                completed = completed.saturating_add(1);
                last_code = result.as_ref().err().map_or(0, |error| error.code);
                tracing::debug!(
                    method = pending.request.log_method(),
                    code = last_code,
                    elapsed_ms = started.elapsed().as_millis(),
                    "RPC request completed"
                );
                last_rpc_unix = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|time| time.as_secs());
                let fatal = result.as_ref().is_err_and(|e| e.fatal);
                let _ = pending.reply.send(result);
                if fatal {
                    return Err(io::Error::other(
                        "RPC storage failure; explicit recovery required",
                    ));
                }
            }
        };
        let result = {
            tokio::pin!(network, owner, shutdown);
            tokio::select! {
                _ = &mut shutdown => Ok(()),
                result = &mut network => result,
                result = &mut owner => result,
            }
        };
        finish_connections(&mut connections, result.is_err()).await;
        self.sync.shutdown().await;
        if let Err(error) = &result {
            tracing::error!(%error, "RPC owner terminated");
        }
        result
    }
}

/// A terminal owner error has already enqueued its diagnostic; let it reach the wire.
///
/// The owner and accept loop have stopped before this drain. No new chain operation
/// or connection can start. A slow reader cannot extend the one-second grace period.
pub(crate) async fn finish_connections(connections: &mut JoinSet<()>, failed: bool) {
    if failed {
        let _ = timeout(Duration::from_secs(1), async {
            while connections.join_next().await.is_some() {}
        })
        .await;
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}
