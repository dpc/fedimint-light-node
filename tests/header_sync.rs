use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use bitcoin::block::Header;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{Hash, sha256d};
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::{BlockHash, CompactTarget, Network};
use fedimint_light_node::fee_policy::FeePolicy;
use fedimint_light_node::header_error::HeaderError;
use fedimint_light_node::header_sync::{HeaderSync, VisitEnd};
use fedimint_light_node::store_anchor::StoreAnchor;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::store_error::StoreError;
use fedimint_light_node::sync_config::SyncConfig;
use fedimint_light_node::sync_error::SyncError;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;

fn headers() -> Vec<Header> {
    include_bytes!("data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .map(|bytes| deserialize(bytes).unwrap())
        .collect()
}

fn storage_config(capacity: u64) -> StoreConfig {
    StoreConfig {
        header_capacity: NonZeroU64::new(capacity).unwrap(),
        cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
    }
}

fn anchor() -> StoreAnchor {
    let headers = headers();
    StoreAnchor {
        network: Network::Bitcoin,
        height: 0,
        hash: headers[0].block_hash(),
        height_one_hash: headers[1].block_hash(),
    }
}

fn config(peers: Vec<SocketAddr>, capacity: usize) -> SyncConfig {
    SyncConfig {
        peer_pool: fedimint_light_node::peer_pool_config::PeerPoolConfig::manual_only(),
        peers,
        handshake_timeout: Duration::from_secs(2),
        request_timeout: Duration::from_secs(2),
        retry_delay: Duration::from_millis(1),
        requests_per_visit: 4,
        candidate_headers: capacity,
    }
}

fn service(dir: &tempfile::TempDir, config: SyncConfig) -> HeaderSync {
    let store = ValidatedHeaderStore::create(
        dir.path().join("chain"),
        storage_config(config.candidate_headers as u64),
        anchor(),
        &headers()[..1],
    )
    .unwrap();
    HeaderSync::new(store, config).unwrap()
}

#[test]
fn guardian_queries_survive_pruning_reopen_without_implicit_protection() {
    let dir = tempfile::tempdir().unwrap();
    let headers = headers();
    let mut store = ValidatedHeaderStore::create(
        dir.path().join("chain"),
        storage_config(2),
        anchor(),
        &headers[..1],
    )
    .unwrap();
    store.select_suffix(0, &headers[1..=2], u64::MAX).unwrap();
    store.select_suffix(2, &headers[3..=4], u64::MAX).unwrap();
    let cfg = config(vec!["127.0.0.1:1".parse().unwrap()], 2);
    let node = HeaderSync::new(store, cfg.clone()).unwrap();
    assert_eq!(node.fee_policy(), FeePolicy::Unavailable);
    assert_eq!(node.status().unwrap().fee_policy.sats_per_kvb(), None);
    assert_eq!(node.block_count().unwrap(), 5);
    assert_eq!(node.chain_id(), headers[1].block_hash());
    assert_eq!(node.block_hash(4).unwrap(), headers[4].block_hash());
    assert!(matches!(
        node.block_hash(1),
        Err(StoreError::OutsideRetainedHistory)
    ));
    assert!(matches!(node.block_hash(5), Err(StoreError::BeyondTip)));
    assert_eq!(node.sync_progress(), None);
    assert_eq!(node.store().protected_height().unwrap(), 0);
    let policy = FeePolicy::Fixed {
        sats_per_kvb: NonZeroU64::new(2500).unwrap(),
    };
    let mut node = node.with_fee_policy(policy);
    assert_eq!(node.status().unwrap().fee_policy, policy);
    assert_eq!(node.fee_policy().sats_per_kvb().unwrap().get(), 2500);
    node.protect_through(3, headers[3].block_hash()).unwrap();
    drop(node);
    let store =
        ValidatedHeaderStore::open_pinned(dir.path().join("chain"), storage_config(2)).unwrap();
    let node = HeaderSync::new(store, cfg).unwrap();
    assert_eq!(node.block_count().unwrap(), 5);
    assert_eq!(node.chain_id(), headers[1].block_hash());
    assert_eq!(node.block_hash(3).unwrap(), headers[3].block_hash());
    assert_eq!(node.store().protected_height().unwrap(), 3);
    assert_eq!(node.fee_policy(), FeePolicy::Unavailable); // Host must reconfigure.
}

#[test]
fn selected_count_checks_overflow_and_includes_height_zero() {
    let mut status = fedimint_light_node::header_sync::SyncStatus {
        fee_policy: FeePolicy::default(),
        height: 0,
        hash: headers()[0].block_hash(),
        generation: 0,
        stopped: false,
    };
    assert_eq!(status.block_count().unwrap(), 1);
    status.height = u64::MAX - 1;
    assert_eq!(status.block_count().unwrap(), u64::MAX);
    status.height = u64::MAX;
    assert!(matches!(
        status.block_count(),
        Err(StoreError::InvalidUpdate(_))
    ));
}

#[test]
fn fixed_fee_policy_has_positive_explicit_units_without_conversion_overflow() {
    assert_eq!(FeePolicy::default().sats_per_kvb(), None);
    assert!(NonZeroU64::new(0).is_none());
    for value in [1, 1000, 2500, u64::MAX] {
        let rate = NonZeroU64::new(value).unwrap();
        assert_eq!(
            FeePolicy::Fixed { sats_per_kvb: rate }.sats_per_kvb(),
            Some(rate)
        );
    }
}

async fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

async fn send(stream: &mut TcpStream, message: NetworkMessage) {
    stream
        .write_all(&serialize(&RawNetworkMessage::new(
            Network::Bitcoin.magic(),
            message,
        )))
        .await
        .unwrap();
}

async fn read(stream: &mut TcpStream) -> NetworkMessage {
    let mut header = [0; 24];
    stream.read_exact(&mut header).await.unwrap();
    let len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
    assert!(len < 2_000_000);
    let mut frame = header.to_vec();
    frame.resize(24 + len, 0);
    stream.read_exact(&mut frame[24..]).await.unwrap();
    deserialize::<RawNetworkMessage>(&frame)
        .unwrap()
        .into_payload()
}

async fn handshake(listener: TcpListener) -> TcpStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    let NetworkMessage::Version(version) = read(&mut stream).await else {
        panic!("expected version");
    };
    assert!(!version.relay);
    assert_eq!(version.services, ServiceFlags::NONE);
    let address = Address::new(&stream.local_addr().unwrap(), ServiceFlags::NETWORK);
    let version = VersionMessage::new(
        ServiceFlags::NETWORK,
        0,
        address.clone(),
        address,
        42,
        "/scripted/".into(),
        i32::MAX, // Must never be mistaken for a trustworthy synchronization target.
    );
    send(&mut stream, NetworkMessage::Version(version)).await;
    send(&mut stream, NetworkMessage::WtxidRelay).await;
    send(&mut stream, NetworkMessage::SendAddrV2).await;
    send(&mut stream, NetworkMessage::Verack).await;
    assert_eq!(read(&mut stream).await, NetworkMessage::Verack);
    stream
}

async fn request(stream: &mut TcpStream) -> Vec<BlockHash> {
    let NetworkMessage::GetHeaders(request) = read(stream).await else {
        panic!("expected getheaders");
    };
    assert_eq!(request.stop_hash, BlockHash::all_zeros());
    assert!(request.locator_hashes.len() <= 80);
    request.locator_hashes
}

async fn closed(stream: &mut TcpStream) {
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(3), stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn handshake_real_headers_ping_pruning_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (listener, addr) = listener().await;
    let server = tokio::spawn(async move {
        let mut stream = handshake(listener).await;
        assert_eq!(request(&mut stream).await[0], headers()[0].block_hash());
        send(&mut stream, NetworkMessage::Ping(99)).await;
        assert_eq!(read(&mut stream).await, NetworkMessage::Pong(99));
        send(
            &mut stream,
            NetworkMessage::Headers(headers()[1..=2000].to_vec()),
        )
        .await;
        assert_eq!(request(&mut stream).await[0], headers()[2000].block_hash());
        send(
            &mut stream,
            NetworkMessage::Headers(headers()[2001..].to_vec()),
        )
        .await;
        assert_eq!(request(&mut stream).await[0], headers()[2035].block_hash());
        send(&mut stream, NetworkMessage::Headers(Vec::new())).await;
        closed(&mut stream).await;
    });
    // A 2000-header response must also work with a tiny rolling/candidate window.
    let mut sync = service(&dir, config(vec![addr], 17));
    let result = sync.step(u64::MAX).await;
    assert_eq!(result.result.unwrap(), VisitEnd::EmptyResponse);
    assert_eq!(sync.status().unwrap().height, 2035);
    assert!(sync.status().unwrap().generation > 1);
    assert_eq!(sync.store().bounds().unwrap(), (2019, 2035));
    sync.shutdown().await;
    server.await.unwrap();
    drop(sync);
    let store =
        ValidatedHeaderStore::open(dir.path().join("chain"), storage_config(17), anchor()).unwrap();
    assert_eq!(store.header(2035).unwrap(), headers()[2035]);
}

#[tokio::test]
async fn withholding_times_out_and_next_peer_progresses() {
    let dir = tempfile::tempdir().unwrap();
    let (slow, slow_addr) = listener().await;
    let (good, good_addr) = listener().await;
    let slow_server = tokio::spawn(async move {
        let mut stream = handshake(slow).await;
        request(&mut stream).await;
        // Deadline cannot be extended by valid chatter.
        for _ in 0..3 {
            send(&mut stream, NetworkMessage::Ping(7)).await;
            assert_eq!(read(&mut stream).await, NetworkMessage::Pong(7));
        }
        closed(&mut stream).await;
    });
    let good_server = tokio::spawn(async move {
        let mut stream = handshake(good).await;
        request(&mut stream).await;
        send(
            &mut stream,
            NetworkMessage::Headers(headers()[1..3].to_vec()),
        )
        .await;
    });
    let mut config = config(vec![slow_addr, good_addr], 8);
    config.request_timeout = Duration::from_millis(150);
    config.requests_per_visit = 1;
    let mut sync = service(&dir, config);
    let report = sync.step(u64::MAX).await;
    assert_eq!(report.peer, Some(slow_addr));
    assert!(matches!(report.result, Err(SyncError::Timeout)));
    assert_eq!(sync.status().unwrap().height, 0);
    let report = sync.step(u64::MAX).await;
    assert_eq!(report.peer, Some(good_addr));
    assert_eq!(report.result.unwrap(), VisitEnd::RequestLimit);
    assert_eq!(sync.status().unwrap().height, 2);
    slow_server.await.unwrap();
    good_server.await.unwrap();
}

#[tokio::test]
async fn cancellation_closes_handshake_and_request_without_detached_work() {
    for during_handshake in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let (listener, addr) = listener().await;
        let (ready, ready_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let mut stream = if during_handshake {
                let (mut stream, _) = listener.accept().await.unwrap();
                read(&mut stream).await;
                stream
            } else {
                let mut stream = handshake(listener).await;
                request(&mut stream).await;
                send(
                    &mut stream,
                    NetworkMessage::Headers(headers()[1..3].to_vec()),
                )
                .await;
                request(&mut stream).await;
                stream
            };
            ready.send(()).unwrap();
            closed(&mut stream).await;
        });
        let mut sync = service(&dir, config(vec![addr], 8));
        tokio::select! {
            _ = ready_rx => {},
            _ = sync.step(u64::MAX) => panic!("visit finished before cancellation"),
        }
        assert_eq!(
            sync.status().unwrap().height,
            if during_handshake { 0 } else { 2 }
        );
        assert!(!sync.status().unwrap().stopped);
        server.await.unwrap();
    }
}

async fn raw(stream: &mut TcpStream, command: &[u8], payload: &[u8], length: Option<u32>) {
    let mut frame = [0; 24];
    frame[..4].copy_from_slice(&Network::Bitcoin.magic().to_bytes());
    frame[4..4 + command.len()].copy_from_slice(command);
    frame[16..20].copy_from_slice(&length.unwrap_or(payload.len() as u32).to_le_bytes());
    frame[20..24].copy_from_slice(&sha256d::Hash::hash(payload)[..4]);
    stream.write_all(&frame).await.unwrap();
    if length.is_none() {
        stream.write_all(payload).await.unwrap();
    }
}

#[tokio::test]
async fn hostile_frames_and_invalid_headers_do_not_block_other_peers() {
    for attack in 0..8 {
        let dir = tempfile::tempdir().unwrap();
        let (bad, bad_addr) = listener().await;
        let (good, good_addr) = listener().await;
        let bad_server = tokio::spawn(async move {
            let mut stream = handshake(bad).await;
            request(&mut stream).await;
            match attack {
                0 => raw(&mut stream, b"headers", &[], Some(162_004)).await,
                1 => raw(&mut stream, b"block", &[], Some(4_000_000)).await,
                2 => {
                    raw(
                        &mut stream,
                        b"headers",
                        &[255, 255, 255, 255, 255, 255, 255, 255, 255],
                        None,
                    )
                    .await
                }
                3 => {
                    let mut bytes = vec![1];
                    bytes.extend(serialize(&headers()[1]));
                    bytes.push(1);
                    raw(&mut stream, b"headers", &bytes, None).await;
                }
                4 => {
                    let mut header = headers()[1];
                    header.bits = CompactTarget::from_consensus(0x1c00ffff);
                    send(&mut stream, NetworkMessage::Headers(vec![header])).await;
                }
                5 => {
                    let mut header = headers()[1];
                    header.prev_blockhash = BlockHash::all_zeros();
                    send(&mut stream, NetworkMessage::Headers(vec![header])).await;
                }
                6 => {
                    for _ in 0..32 {
                        send(&mut stream, NetworkMessage::SendHeaders).await;
                    }
                }
                7 => raw(&mut stream, b"headers", &[253, 0, 0], None).await,
                _ => unreachable!(),
            }
            closed(&mut stream).await;
        });
        let good_server = tokio::spawn(async move {
            let mut stream = handshake(good).await;
            request(&mut stream).await;
            send(
                &mut stream,
                NetworkMessage::Headers(headers()[1..3].to_vec()),
            )
            .await;
        });
        let mut config = config(vec![bad_addr, good_addr], 8);
        config.requests_per_visit = 1;
        let mut sync = service(&dir, config);
        let result = sync.step(u64::MAX).await.result;
        if attack == 4 {
            assert!(matches!(
                result,
                Err(SyncError::Header(HeaderError::Difficulty))
            ));
        } else if attack == 5 {
            assert!(matches!(result, Err(SyncError::UnsupportedCandidate)));
        } else {
            assert!(matches!(result, Err(SyncError::Protocol(_))), "{result:?}");
        }
        assert_eq!(sync.status().unwrap().height, 0);
        assert!(!sync.status().unwrap().stopped);
        sync.step(u64::MAX).await.result.unwrap();
        assert_eq!(sync.status().unwrap().height, 2);
        bad_server.await.unwrap();
        good_server.await.unwrap();
    }
}

#[tokio::test]
async fn retained_fork_accumulates_across_batches_and_respects_protection() {
    let mainnet: Vec<Header> = include_bytes!("data/mainnet-headers-477792-478562.raw")
        .chunks_exact(80)
        .map(|bytes| deserialize(bytes).unwrap())
        .collect();
    let fork: Vec<Header> = include_bytes!("data/bch-headers-478559-478562.raw")
        .chunks_exact(80)
        .map(|bytes| deserialize(bytes).unwrap())
        .collect();
    for protected in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let anchor = StoreAnchor {
            network: Network::Bitcoin,
            height: 478_558,
            hash: mainnet[766].block_hash(),
            height_one_hash: anchor().height_one_hash,
        };
        let mut store = ValidatedHeaderStore::create(
            dir.path().join("chain"),
            storage_config(5),
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
        let (listener, addr) = listener().await;
        let branch = fork.clone();
        let server = tokio::spawn(async move {
            let mut stream = handshake(listener).await;
            assert!(request(&mut stream).await.contains(&anchor.hash));
            send(&mut stream, NetworkMessage::Headers(branch[..1].to_vec())).await;
            if !protected {
                assert_eq!(request(&mut stream).await, vec![branch[0].block_hash()]);
                send(&mut stream, NetworkMessage::Headers(branch[1..3].to_vec())).await;
                assert_eq!(request(&mut stream).await[0], branch[2].block_hash());
                send(&mut stream, NetworkMessage::Headers(Vec::new())).await;
            }
            closed(&mut stream).await;
        });
        let mut sync = HeaderSync::new(store, config(vec![addr], 5)).unwrap();
        let result = sync.step(u64::MAX).await.result;
        if protected {
            assert!(matches!(result, Err(SyncError::UnsupportedCandidate)));
            assert!(!sync.status().unwrap().stopped);
            assert_eq!(sync.store().header(478_559).unwrap(), mainnet[767]);
        } else {
            assert_eq!(result.unwrap(), VisitEnd::EmptyResponse);
            assert_eq!(sync.store().header(478_559).unwrap(), fork[0]);
            assert_eq!(sync.status().unwrap().generation, 1);
        }
        sync.shutdown().await;
        server.await.unwrap();
        drop(sync);
        let reopened =
            ValidatedHeaderStore::open(dir.path().join("chain"), storage_config(5), anchor)
                .unwrap();
        assert_eq!(
            reopened.bounds().unwrap().1,
            if protected { 478_560 } else { 478_561 }
        );
    }
}

#[tokio::test]
async fn handshake_rejects_hostile_version_lengths_and_missing_verack() {
    for attack in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let (listener, addr) = listener().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(matches!(
                read(&mut stream).await,
                NetworkMessage::Version(_)
            ));
            match attack {
                0 => {
                    let mut payload = vec![0; 80];
                    payload.extend([255; 9]);
                    raw(&mut stream, b"version", &payload, None).await;
                }
                1 => raw(&mut stream, b"version", &[], Some(u32::MAX)).await,
                2 => send(&mut stream, NetworkMessage::Verack).await,
                _ => unreachable!(),
            }
            closed(&mut stream).await;
        });
        let mut sync = service(&dir, config(vec![addr], 8));
        assert!(matches!(
            sync.step(u64::MAX).await.result,
            Err(SyncError::Protocol(_))
        ));
        assert_eq!(sync.status().unwrap().height, 0);
        assert!(!sync.status().unwrap().stopped);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn ignored_announcements_have_an_aggregate_byte_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (listener, addr) = listener().await;
    let server = tokio::spawn(async move {
        let mut stream = handshake(listener).await;
        request(&mut stream).await;
        // Ignored announcements are streamed and checksummed, never decoded into
        // an attacker-sized inventory vector. Second envelope exceeds the exchange.
        raw(&mut stream, b"inv", &vec![0; 1_800_003], None).await;
        raw(&mut stream, b"inv", &[], Some(1_800_003)).await;
        closed(&mut stream).await;
    });
    let mut sync = service(&dir, config(vec![addr], 8));
    assert!(matches!(
        sync.step(u64::MAX).await.result,
        Err(SyncError::Protocol(_))
    ));
    assert_eq!(sync.status().unwrap().generation, 0);
    server.await.unwrap();
}

#[test]
fn rejects_invalid_global_budgets() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(vec!["127.0.0.1:8333".parse().unwrap()], 8);
    config.peers.resize(17, config.peers[0]);
    let store = ValidatedHeaderStore::create(
        dir.path().join("chain"),
        storage_config(8),
        anchor(),
        &headers()[..1],
    )
    .unwrap();
    assert!(matches!(
        HeaderSync::new(store, config),
        Err(SyncError::Configuration)
    ));
}
