use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use bitcoin::consensus::serialize;
use bitcoin::hex::DisplayHex;

use super::*;
use crate::explorer_header_config::ExplorerHeaderConfig;
use crate::explorer_test_support::{Server, elapse, headers, reply};
use crate::store_anchor::StoreAnchor;
use crate::store_config::StoreConfig;

fn storage(cap: usize) -> StoreConfig {
    StoreConfig {
        header_capacity: NonZeroU64::new(cap as u64).unwrap(),
        cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
    }
}

fn service(dir: &tempfile::TempDir, roots: Vec<String>, cap: usize) -> HeaderSync {
    let hs = headers();
    let store = ValidatedHeaderStore::create(
        dir.path().join("chain"),
        storage(cap),
        StoreAnchor {
            network: bitcoin::Network::Bitcoin,
            height: 0,
            hash: hs[0].block_hash(),
            height_one_hash: hs[1].block_hash(),
        },
        &hs[..1],
    )
    .unwrap();
    HeaderSync::new(
        store,
        SyncConfig {
            peers: vec!["127.0.0.1:1".parse().unwrap()],
            peer_pool: crate::peer_pool_config::PeerPoolConfig::manual_only(),
            handshake_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            retry_delay: Duration::from_millis(1),
            requests_per_visit: 1,
            candidate_headers: cap,
        },
    )
    .unwrap()
    .with_explorer_headers(ExplorerHeaderConfig { roots })
    .unwrap()
}

fn due(sync: &mut HeaderSync) {
    for source in &mut sync.explorers.as_mut().unwrap().sources {
        source.due = tokio::time::Instant::now();
    }
}

async fn turn(sync: &mut HeaderSync) {
    elapse(Duration::from_secs(1)).await;
    sync.explorer_step(u64::MAX).await.unwrap();
}

#[tokio::test]
async fn independent_http_advances_and_reopens_despite_failed_source() {
    let bad =
        Server::new(|_| b"HTTP/1.1 503 Unavailable\r\nContent-Length: 999999\r\n\r\n".to_vec())
            .await;
    let good = Server::chain(3).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![bad.root.clone(), good.root.clone()], 256);
    let (peer, peer_task) = crate::explorer_test_support::withholding_peer().await;
    sync.pool = PeerPool::new(
        &[peer],
        crate::peer_pool_config::PeerPoolConfig::manual_only(),
    )
    .unwrap();
    assert!(matches!(
        sync.step(u64::MAX).await.result,
        Ok(VisitEnd::EmptyResponse)
    ));
    assert_eq!(sync.status().unwrap().height, 0);
    due(&mut sync);
    for _ in 0..5 {
        turn(&mut sync).await;
    }
    let status = sync.status().unwrap();
    assert_eq!(
        (
            status.height,
            status.hash,
            status.generation,
            status.stopped
        ),
        (3, headers()[3].block_hash(), 1, false)
    );
    assert_eq!(bad.paths.lock().unwrap().len(), 1);
    assert_eq!(good.paths.lock().unwrap().len(), 4);
    sync.shutdown().await;
    assert_eq!(peer_task.await.unwrap(), 1);
    drop(sync);
    let reopened =
        ValidatedHeaderStore::open_pinned(dir.path().join("chain"), storage(256)).unwrap();
    assert_eq!(reopened.header(3).unwrap(), headers()[3]);
}

#[tokio::test]
async fn cli_five_second_pacing_services_explorers_without_canceling_dial() {
    let server = Server::chain(3).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![server.root.clone()], 256);
    let (peer, peer_task) = crate::explorer_test_support::withholding_peer().await;
    sync.pool = PeerPool::new(
        &[peer],
        crate::peer_pool_config::PeerPoolConfig::manual_only(),
    )
    .unwrap();
    sync.config.retry_delay = Duration::from_secs(5);
    due(&mut sync);
    // All four HTTP requests complete during the original wait, while the
    // configured five-second owner + five-second pool pacing still reaches dial.
    let start = tokio::time::Instant::now();
    let report = tokio::time::timeout(Duration::from_secs(13), sync.step_with_explorers(u64::MAX))
        .await
        .unwrap();
    assert!(matches!(report.result, Ok(VisitEnd::EmptyResponse)));
    assert!(start.elapsed() >= Duration::from_secs(10));
    assert_eq!(sync.status().unwrap().height, 3);
    assert_eq!(server.paths.lock().unwrap().len(), 4);
    sync.shutdown().await;
    assert_eq!(peer_task.await.unwrap(), 1);
}

#[tokio::test]
async fn due_explorer_yields_after_nonempty_exchange_and_keeps_peer_reusable() {
    use crate::explorer_test_support::{p2p_handshake, p2p_read, p2p_send};
    use bitcoin::p2p::message::NetworkMessage;
    let server = Server::chain(3).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![server.root.clone()], 256);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    sync.pool = PeerPool::new(
        &[listener.local_addr().unwrap()],
        crate::peer_pool_config::PeerPoolConfig::manual_only(),
    )
    .unwrap();
    sync.config.requests_per_visit = 16;
    let peer = tokio::spawn(async move {
        let mut stream = p2p_handshake(&listener).await;
        assert!(matches!(
            p2p_read(&mut stream).await,
            Some(NetworkMessage::GetHeaders(_))
        ));
        p2p_send(&mut stream, NetworkMessage::Headers(vec![headers()[1]])).await;
        // A normal clean-boundary release retains the same connection.
        assert!(matches!(
            p2p_read(&mut stream).await,
            Some(NetworkMessage::GetHeaders(_))
        ));
        p2p_send(&mut stream, NetworkMessage::Headers(vec![])).await;
        assert!(p2p_read(&mut stream).await.is_none());
    });
    let mut received = 2_000_000;
    let mut sent = usize::MAX;
    let mut connection = sync
        .pool
        .acquire(
            0,
            sync.config.retry_delay,
            sync.config.handshake_timeout,
            &mut received,
            &mut sent,
            true,
        )
        .await
        .unwrap();
    // Synchronize at the established-connection boundary instead of assuming
    // setup/handshake completes within an arbitrary wall-clock phase.
    due(&mut sync);
    let mut useful = false;
    assert!(matches!(
        sync.visit(&mut connection, u64::MAX, &mut useful, true)
            .await,
        Ok(VisitEnd::Maintenance)
    ));
    sync.pool.release(connection, useful);
    assert_eq!(sync.status().unwrap().height, 1);
    assert!(server.paths.lock().unwrap().is_empty());
    assert!(matches!(
        sync.step_with_explorers(u64::MAX).await.result,
        Ok(VisitEnd::EmptyResponse)
    ));
    assert_eq!(server.paths.lock().unwrap().len(), 1);
    for _ in 0..2 {
        turn(&mut sync).await;
    }
    assert_eq!(sync.status().unwrap().height, 3);
    sync.shutdown().await;
    peer.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn canceled_long_pacing_does_not_restart_pool_deadline_or_charge_peer() {
    let mut pool = PeerPool::new(
        &["127.0.0.1:1".parse().unwrap()],
        crate::peer_pool_config::PeerPoolConfig::manual_only(),
    )
    .unwrap();
    let start = tokio::time::Instant::now();
    let due = pool
        .prepare_acquire(Duration::from_secs(3600), Duration::from_secs(2), true)
        .await;
    assert_eq!(due, start + Duration::from_secs(3600));
    for _ in 0..4 {
        tokio::time::advance(Duration::from_secs(900)).await;
        assert_eq!(
            pool.prepare_acquire(Duration::from_secs(3600), Duration::from_secs(2), true)
                .await,
            due
        );
        assert_eq!(pool.counts().0, 0);
    }
    // Readiness is now reached; it does not schedule yet another hour.
    let mut sent = usize::MAX;
    let result = pool
        .acquire_prepared(
            0,
            Duration::from_secs(3600),
            Duration::from_secs(2),
            &mut 2_000_000,
            &mut sent,
        )
        .await;
    assert!(result.is_err()); // local closed port, not a pacing timeout
    assert!(tokio::time::Instant::now() < due + Duration::from_secs(3));
}

#[tokio::test]
async fn busy_rpc_and_withholding_p2p_cannot_starve_full_http_requests() {
    use crate::guardian_rpc::GuardianRpc;
    use crate::guardian_rpc_config::{GuardianRpcConfig, RpcAuth};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let server = Server::delayed_chain(2, Duration::from_millis(1500)).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![server.root.clone()], 256);
    let (peer, peer_task) = crate::explorer_test_support::withholding_peer().await;
    sync.pool = PeerPool::new(
        &[peer],
        crate::peer_pool_config::PeerPoolConfig::manual_only(),
    )
    .unwrap();
    due(&mut sync);
    let rpc = GuardianRpc::bind(
        GuardianRpcConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            auth: RpcAuth::NoAuth,
            fake_bitcoind: false,
            allow_submission: false,
        },
        sync,
    )
    .await
    .unwrap();
    let addr = rpc.local_addr().unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    let mut send = Some(send);
    let owner = tokio::spawn(rpc.run(
        async {
            let _ = receive.await;
        },
        move |sync, _| {
            if sync.status().unwrap().height == 2
                && let Some(send) = send.take()
            {
                send.send(()).unwrap();
            }
        },
    ));
    let completed = Arc::new(AtomicUsize::new(0));
    let mut clients = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let completed = Arc::clone(&completed);
        clients.spawn(async move {
            loop {
                let Ok(mut stream) = tokio::net::TcpStream::connect(addr).await else { return };
                let body = r#"{"jsonrpc":"1.0","id":1,"method":"getblockcount","params":[]}"#;
                let request = format!("POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                if stream.write_all(request.as_bytes()).await.is_err() { return; }
                let mut response = Vec::new();
                if stream.read_to_end(&mut response).await.is_err() { return; }
                if String::from_utf8_lossy(&response).contains("\"result\":") {
                    completed.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }
    tokio::time::timeout(Duration::from_secs(14), owner)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    assert!(completed.load(Ordering::Relaxed) >= 3);
    assert_eq!(server.paths.lock().unwrap().len(), 3);
    assert!(peer_task.await.unwrap() >= 2);
    let reopened =
        ValidatedHeaderStore::open_pinned(dir.path().join("chain"), storage(256)).unwrap();
    assert_eq!(reopened.header(2).unwrap(), headers()[2]);
}

#[tokio::test]
async fn queued_reads_get_a_turn_between_slow_p2p_and_http_operations() {
    use crate::explorer_test_support::{p2p_handshake, p2p_read};
    use crate::guardian_rpc::GuardianRpc;
    use crate::guardian_rpc_config::{GuardianRpcConfig, RpcAuth};
    use bitcoin::p2p::message::NetworkMessage;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let server = Server::delayed_chain(1, Duration::from_millis(2400)).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![server.root.clone()], 256);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    sync.pool = PeerPool::new(
        &[listener.local_addr().unwrap()],
        crate::peer_pool_config::PeerPoolConfig::manual_only(),
    )
    .unwrap();
    sync.config.request_timeout = Duration::from_secs(20);
    due(&mut sync);
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut ready = Some(ready);
        loop {
            let mut stream = p2p_handshake(&listener).await;
            assert!(matches!(
                p2p_read(&mut stream).await,
                Some(NetworkMessage::GetHeaders(_))
            ));
            if let Some(ready) = ready.take() {
                ready.send(()).unwrap();
            }
            // No response: each peer slice consumes the owner's full four seconds.
            assert!(p2p_read(&mut stream).await.is_none());
        }
    });
    let rpc = GuardianRpc::bind(
        GuardianRpcConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            auth: RpcAuth::NoAuth,
            fake_bitcoind: false,
            allow_submission: false,
        },
        sync,
    )
    .await
    .unwrap();
    let addr = rpc.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let owner = tokio::spawn(rpc.run(
        async {
            let _ = stopped.await;
        },
        |_, _| {},
    ));
    waiting.await.unwrap();
    let mut clients = tokio::task::JoinSet::new();
    for _ in 0..2 {
        clients.spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let body = r#"{"jsonrpc":"1.0","id":1,"method":"getblockcount","params":[]}"#;
            stream.write_all(format!("POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            let response = String::from_utf8(response).unwrap();
            let body = response.split_once("\r\n\r\n").unwrap().1;
            let value: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(value["error"], serde_json::Value::Null, "{value}");
            assert_eq!(value["result"], 0);
        });
    }
    // Two old P2P+HTTP pairs took 12.8s, exceeding the queue's 11s wait.
    // Alternation serves these reads after 4s and 6.4s respectively.
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(result) = clients.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    owner.await.unwrap().unwrap();
    peer.abort();
    assert_eq!(server.paths.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn pinned_snapshot_resumes_across_visit_and_request_caps() {
    let server = Server::chain(20).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![server.root.clone()], 256);
    due(&mut sync);
    for _ in 0..16 {
        turn(&mut sync).await;
    }
    assert_eq!(sync.status().unwrap().height, 0);
    assert_eq!(
        sync.explorers.as_ref().unwrap().sources[0].reverse.len(),
        15
    );
    assert!(!sync.explorer_step(u64::MAX).await.unwrap());
    elapse(Duration::from_secs(2701)).await;
    for _ in 0..5 {
        turn(&mut sync).await;
    }
    assert_eq!(sync.status().unwrap().height, 20);
    assert_eq!(
        server
            .paths
            .lock()
            .unwrap()
            .iter()
            .filter(|p| *p == "/blocks/tip/hash")
            .count(),
        1
    );
}

#[tokio::test]
async fn depth_limit_is_nonterminal_and_does_not_reanchor() {
    let server = Server::chain(5).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![server.root.clone()], 3);
    due(&mut sync);
    for _ in 0..4 {
        turn(&mut sync).await;
    }
    assert_eq!(sync.status().unwrap().height, 0);
    assert!(!sync.status().unwrap().stopped);
    assert!(
        sync.explorers.as_ref().unwrap().sources[0]
            .reverse
            .is_empty()
    );
    assert_eq!(sync.explorers.as_ref().unwrap().cap, 3);
    assert!(!sync.explorer_step(u64::MAX).await.unwrap());
}

#[tokio::test]
async fn exact_depth_cap_can_connect_and_select() {
    let server = Server::chain(3).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![server.root.clone()], 3);
    due(&mut sync);
    for _ in 0..4 {
        turn(&mut sync).await;
    }
    assert_eq!(sync.status().unwrap().height, 3);
    assert_eq!(sync.status().unwrap().generation, 1);
}

#[tokio::test]
async fn explorer_forks_obey_work_and_protection_at_shared_admission() {
    let mainnet: Vec<Header> = include_bytes!("../tests/data/mainnet-headers-477792-478562.raw")
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect();
    let fork: Vec<Header> = include_bytes!("../tests/data/bch-headers-478559-478562.raw")
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect();
    for (length, protected) in [(1, false), (2, false), (3, false), (3, true)] {
        let branch = fork[..length].to_vec();
        let server = Server::new(move |path| {
            if path == "/blocks/tip/hash" {
                return reply(&branch.last().unwrap().block_hash().to_string());
            }
            let header = branch
                .iter()
                .find(|header| path == format!("/block/{}/header", header.block_hash()))
                .unwrap();
            reply(&serialize(header).to_lower_hex_string())
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let anchor = StoreAnchor {
            network: bitcoin::Network::Bitcoin,
            height: 478_558,
            hash: mainnet[766].block_hash(),
            height_one_hash: headers()[1].block_hash(),
        };
        let mut store = ValidatedHeaderStore::create(
            dir.path().join("chain"),
            storage(5),
            anchor,
            &mainnet[..767],
        )
        .unwrap();
        store
            .select_suffix(anchor.height, &mainnet[767..769], u64::MAX)
            .unwrap();
        if protected {
            store
                .protect_through(478_559, mainnet[767].block_hash())
                .unwrap();
        }
        let mut sync = HeaderSync::new(
            store,
            SyncConfig {
                peers: vec!["127.0.0.1:1".parse().unwrap()],
                peer_pool: crate::peer_pool_config::PeerPoolConfig::manual_only(),
                handshake_timeout: Duration::from_secs(1),
                request_timeout: Duration::from_secs(1),
                retry_delay: Duration::from_millis(1),
                requests_per_visit: 1,
                candidate_headers: 5,
            },
        )
        .unwrap()
        .with_explorer_headers(ExplorerHeaderConfig {
            roots: vec![server.root.clone()],
        })
        .unwrap();
        due(&mut sync);
        for _ in 0..=length {
            turn(&mut sync).await;
        }
        let selected = length == 3 && !protected;
        assert_eq!(sync.status().unwrap().generation, u64::from(selected));
        assert!(!sync.status().unwrap().stopped);
        assert_eq!(
            sync.store.header(478_559).unwrap(),
            if selected { fork[0] } else { mainnet[767] }
        );
    }
}

#[tokio::test]
async fn current_parent_is_resolved_after_intervening_selection_and_protection() {
    let server = Server::chain(3).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![server.root.clone()], 3);
    due(&mut sync);
    turn(&mut sync).await; // tip hash
    turn(&mut sync).await; // header 3
    // P2P-equivalent common selection boundary, including a protected/pruned parent.
    sync.select_candidate(&mut 0, &mut headers()[1..=2].to_vec(), u64::MAX)
        .unwrap();
    sync.protect_through(2, headers()[2].block_hash()).unwrap();
    turn(&mut sync).await;
    assert_eq!(sync.status().unwrap().height, 3);
    assert_eq!(sync.status().unwrap().generation, 2);
    assert!(!sync.status().unwrap().stopped);
}

#[tokio::test]
async fn malformed_and_wrong_hash_are_source_local() {
    for body in [
        "00".into(),
        format!("{} ", headers()[1].block_hash()),
        serialize(&headers()[2]).to_lower_hex_string(),
    ] {
        let tip = headers()[1].block_hash();
        let server = Server::new(move |path| {
            if path == "/blocks/tip/hash" {
                reply(&tip.to_string())
            } else {
                reply(&body)
            }
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut sync = service(&dir, vec![server.root.clone()], 256);
        due(&mut sync);
        turn(&mut sync).await;
        turn(&mut sync).await;
        assert_eq!(sync.status().unwrap().generation, 0);
        assert!(!sync.status().unwrap().stopped);
    }
}

#[tokio::test]
async fn invalid_context_never_partially_publishes_connected_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec!["http://127.0.0.1:1".into()], 256);
    // Valid hashes and PoW for both, but wrong chain position: validation must
    // reject the entire suffix rather than first publishing the valid block 1.
    let source = &mut sync.explorers.as_mut().unwrap().sources[0];
    source.reverse = vec![headers()[3]];
    source.expected = Some(headers()[1].block_hash());
    let result = sync.accept_explorer_reply(
        0,
        Some(headers()[1].block_hash()),
        &serialize(&headers()[1]).to_lower_hex_string(),
        u64::MAX,
    );
    assert!(matches!(
        result,
        Err(SyncError::Header(HeaderError::Linkage))
    ));
    assert_eq!(sync.status().unwrap().generation, 0);
    assert_eq!(sync.status().unwrap().height, 0);
}

#[tokio::test]
async fn cancellation_keeps_cursor_and_request_reservations() {
    let server = Server::chain(20).await;
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service(&dir, vec![server.root.clone()], 256);
    due(&mut sync);
    turn(&mut sync).await;
    turn(&mut sync).await;
    // A polled, then dropped request must consume both pacing and visit count.
    for _ in 0..14 {
        elapse(Duration::from_secs(1)).await;
        let future = sync.explorer_step(u64::MAX);
        tokio::pin!(future);
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert_eq!(sync.explorers.as_ref().unwrap().sources[0].reverse.len(), 1);
    elapse(Duration::from_secs(1)).await;
    assert!(!sync.explorer_step(u64::MAX).await.unwrap());
}
