use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use bitcoin::block::Header;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hex::DisplayHex;
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_network::VersionMessage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::bootstrap_config::BootstrapSource;
use crate::header_sync::{HeaderSync, VisitEnd};
use crate::store_config::StoreConfig;
use crate::store_error::StoreError;
use crate::sync_config::SyncConfig;

fn headers() -> Vec<Header> {
    include_bytes!("../tests/data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .map(|b| deserialize(b).unwrap())
        .collect()
}

/// Test server mode; production has no cleartext transport override.
#[derive(Clone, Copy)]
enum Fault {
    None,
    Disagree,
    WrongNetwork,
    Malformed,
    Oversize,
    Truncate,
    Redirect,
    Encoded,
    Timeout,
}

async fn source(tip: u64, fault: Fault) -> (BootstrapSource, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
                assert!(request.len() < 4096);
            }
            let request = String::from_utf8(request).unwrap();
            let path = request.split_whitespace().nth(1).unwrap();
            let hs = headers();
            let mut body = if path == "/blocks/tip/height" {
                tip.to_string()
            } else if let Some(height) = path.strip_prefix("/block-height/") {
                let height: usize = height.parse().unwrap();
                let mut hash = hs[height].block_hash();
                if matches!(fault, Fault::WrongNetwork)
                    || (matches!(fault, Fault::Disagree) && height > 1)
                {
                    hash = hs[2].block_hash();
                }
                hash.to_string()
            } else {
                let hash = path
                    .strip_prefix("/block/")
                    .unwrap()
                    .strip_suffix("/header")
                    .unwrap();
                let header = hs
                    .iter()
                    .find(|h| h.block_hash().to_string() == hash)
                    .unwrap();
                serialize(header).to_lower_hex_string()
            };
            match fault {
                Fault::Malformed => body = "not-a-hash".into(),
                Fault::Oversize => body = "0".repeat(1000),
                Fault::Timeout => {
                    std::future::pending::<()>().await;
                }
                _ => {}
            }
            let status = if matches!(fault, Fault::Redirect) {
                "302 Found"
            } else {
                "200 OK"
            };
            let length = body.len();
            if matches!(fault, Fault::Truncate) {
                body.pop();
            }
            let encoding = if matches!(fault, Fault::Encoded) {
                "Content-Encoding: gzip\r\n"
            } else {
                ""
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {length}\r\n{encoding}Connection: close\r\n\r\n{body}"
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (
        BootstrapSource {
            operator: addr.to_string(),
            api: format!("http://{addr}"),
        },
        handle,
    )
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
    let mut envelope = [0; 24];
    stream.read_exact(&mut envelope).await.unwrap();
    let len = u32::from_le_bytes(envelope[16..20].try_into().unwrap()) as usize;
    assert!(len < 200_000);
    let mut bytes = envelope.to_vec();
    bytes.resize(24 + len, 0);
    stream.read_exact(&mut bytes[24..]).await.unwrap();
    deserialize::<RawNetworkMessage>(&bytes)
        .unwrap()
        .into_payload()
}

async fn peer_handshake(listener: &TcpListener) -> TcpStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
        read(&mut stream).await,
        NetworkMessage::Version(_)
    ));
    let address = Address::new(&stream.local_addr().unwrap(), ServiceFlags::NETWORK);
    send(
        &mut stream,
        NetworkMessage::Version(VersionMessage::new(
            ServiceFlags::NETWORK,
            0,
            address.clone(),
            address,
            0,
            "/bootstrap-test/".into(),
            2035,
        )),
    )
    .await;
    send(&mut stream, NetworkMessage::Verack).await;
    assert_eq!(read(&mut stream).await, NetworkMessage::Verack);
    stream
}

fn config(
    sources: Vec<BootstrapSource>,
    peer: std::net::SocketAddr,
    capacity: u64,
) -> BootstrapConfig {
    BootstrapConfig {
        sources,
        peers: vec![peer],
        store: StoreConfig {
            header_capacity: NonZeroU64::new(capacity).unwrap(),
            cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
        },
        request_timeout: Duration::from_millis(500),
        total_timeout: Duration::from_secs(10),
        ..BootstrapConfig::default()
    }
}

#[tokio::test]
async fn common_height_history_offline_reopen_then_sync_across_retarget() {
    let (a, ah) = source(2021, Fault::None).await;
    let (b, bh) = source(2025, Fault::None).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = listener.local_addr().unwrap();
    let serving = tokio::spawn(async move {
        let hs = headers();
        let mut stream = peer_handshake(&listener).await;
        // Anchor 2015 needs context from zero, despite a ten-header serving window.
        for (start, end) in [(1, 2000), (2001, 2015)] {
            let NetworkMessage::GetHeaders(request) = read(&mut stream).await else {
                panic!()
            };
            assert_eq!(request.locator_hashes, vec![hs[start - 1].block_hash()]);
            assert_eq!(request.stop_hash, hs[2015].block_hash());
            send(
                &mut stream,
                NetworkMessage::Headers(hs[start..=end].to_vec()),
            )
            .await;
        }
        drop(stream);
        let mut stream = peer_handshake(&listener).await;
        assert!(matches!(
            read(&mut stream).await,
            NetworkMessage::GetHeaders(_)
        ));
        send(
            &mut stream,
            NetworkMessage::Headers(hs[2016..=2035].to_vec()),
        )
        .await;
        assert!(matches!(
            read(&mut stream).await,
            NetworkMessage::GetHeaders(_)
        ));
        send(&mut stream, NetworkMessage::Headers(Vec::new())).await;
    });
    let config = config(vec![a, b], peer, 10);
    let store_config = config.store;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain");
    let now = u64::from(headers()[2035].time);
    let store = bootstrap(&path, config, now).await.unwrap();
    assert_eq!(store.bounds().unwrap(), (2006, 2015));
    assert_eq!(store.anchor().height, 2015);
    assert_eq!(store.header(2006).unwrap(), headers()[2006]);
    assert_eq!(
        store.tip_context().unwrap().relative_work(),
        bitcoin::Work::from_be_bytes([0; 32])
    );
    let anchor = store.anchor();
    drop(store);
    ah.abort();
    bh.abort();
    let mut store = ValidatedHeaderStore::open_pinned(&path, store_config).unwrap();
    assert_eq!(store.anchor(), anchor);
    assert!(matches!(
        store.select_suffix(2014, &headers()[2015..2016], now),
        Err(HeaderError::RecoveryRequired)
    ));
    let mut sync = HeaderSync::new(
        store,
        SyncConfig {
            peers: vec![peer],
            handshake_timeout: Duration::from_secs(2),
            request_timeout: Duration::from_secs(2),
            retry_delay: Duration::from_millis(1),
            requests_per_visit: 2,
            candidate_headers: 10,
        },
    )
    .unwrap();
    assert_eq!(
        sync.step(now).await.result.unwrap(),
        VisitEnd::EmptyResponse
    );
    assert_eq!(sync.status().unwrap().height, 2035);
    drop(sync);
    let store = ValidatedHeaderStore::open_pinned(&path, store_config).unwrap();
    assert_eq!(store.bounds().unwrap(), (2026, 2035));
    assert_eq!(store.anchor(), anchor);
    assert!(matches!(
        store.header(2006),
        Err(StoreError::OutsideRetainedHistory)
    ));
    serving.await.unwrap();
}

#[tokio::test]
async fn source_failures_never_publish_a_store() {
    for fault in [
        Fault::Disagree,
        Fault::WrongNetwork,
        Fault::Malformed,
        Fault::Oversize,
        Fault::Truncate,
        Fault::Redirect,
        Fault::Encoded,
        Fault::Timeout,
    ] {
        let (a, ah) = source(2021, Fault::None).await;
        let (b, bh) = source(2025, fault).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain");
        let result = bootstrap(
            &path,
            config(vec![a, b], "127.0.0.1:1".parse().unwrap(), 10),
            u64::from(headers()[2015].time),
        )
        .await;
        let error = result.err().expect("must reject");
        assert!(
            match fault {
                Fault::Disagree => matches!(error, BootstrapError::Disagreement),
                Fault::WrongNetwork => matches!(error, BootstrapError::WrongNetwork),
                Fault::Oversize => matches!(error, BootstrapError::Limit(_)),
                Fault::Truncate => matches!(error, BootstrapError::Http(_)),
                Fault::Timeout => matches!(error, BootstrapError::Timeout),
                _ => matches!(error, BootstrapError::Reply(_)),
            },
            "{error:?}"
        );
        assert!(!path.exists());
        ah.abort();
        bh.abort();
    }
}

#[tokio::test]
async fn stale_future_and_existing_destinations_fail_without_publication() {
    for now in [0, u64::from(headers()[2015].time) + 3 * 86400] {
        let (a, ah) = source(2021, Fault::None).await;
        let (b, bh) = source(2021, Fault::None).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain");
        let cfg = config(vec![a, b], "127.0.0.1:1".parse().unwrap(), 10);
        let error = bootstrap(&path, cfg.clone(), now).await.err().unwrap();
        assert!(matches!(
            error,
            BootstrapError::Stale | BootstrapError::Header(HeaderError::FutureTime)
        ));
        assert!(!path.exists());
        std::fs::write(&path, b"do not overwrite").unwrap();
        assert!(matches!(
            bootstrap(&path, cfg, now).await,
            Err(BootstrapError::AlreadyExists)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"do not overwrite");
        ah.abort();
        bh.abort();
    }
}

#[tokio::test]
async fn cancellation_during_network_work_leaves_no_destination() {
    let (a, ah) = source(2021, Fault::Timeout).await;
    let (b, bh) = source(2021, Fault::None).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain");
    assert!(
        timeout(
            Duration::from_millis(30),
            bootstrap(
                &path,
                config(vec![a, b], "127.0.0.1:1".parse().unwrap(), 10),
                0
            )
        )
        .await
        .is_err()
    );
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    ah.abort();
    bh.abort();
}

#[test]
fn authenticated_history_is_distinct_from_work_and_rejects_forgery_or_overwrite() {
    let hs = headers();
    assert_eq!(height_one_hash(), hs[1].block_hash());
    let anchor = StoreAnchor {
        network: Network::Bitcoin,
        height: 2030,
        hash: hs[2030].block_hash(),
        height_one_hash: height_one_hash(),
    };
    let cfg = StoreConfig {
        header_capacity: NonZeroU64::new(2031).unwrap(),
        cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain");
    let mut forged = hs[..=2030].to_vec();
    forged[50].nonce ^= 1;
    assert!(matches!(
        ValidatedHeaderStore::create_with_history(&path, cfg, anchor, &forged),
        Err(HeaderError::Linkage)
    ));
    assert!(!path.exists());
    assert!(ValidatedHeaderStore::create_with_history(&path, cfg, anchor, &hs[..2030]).is_err());
    let store =
        ValidatedHeaderStore::create_with_history(&path, cfg, anchor, &hs[..=2030]).unwrap();
    assert_eq!(store.bounds().unwrap(), (0, 2030));
    drop(store);
    assert!(ValidatedHeaderStore::create_with_history(&path, cfg, anchor, &hs[..=2030]).is_err());
    let store = ValidatedHeaderStore::open_pinned(&path, cfg).unwrap();
    assert_eq!(store.bounds().unwrap(), (0, 2030));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn configuration_enforces_independence_https_and_finite_resources() {
    let mut cfg = BootstrapConfig {
        peers: vec!["127.0.0.1:1".parse().unwrap()],
        ..BootstrapConfig::default()
    };
    assert!(cfg.valid());
    assert!(BootstrapHttp::new(&cfg).is_ok());
    cfg.sources[1].api = "http://example.com/api".into();
    assert!(BootstrapHttp::new(&cfg).is_err());
    cfg.sources[1].api = cfg.sources[0].api.clone();
    assert!(BootstrapHttp::new(&cfg).is_err());
    cfg.sources[1].operator = cfg.sources[0].operator.clone();
    assert!(!cfg.valid());
    cfg = BootstrapConfig {
        peers: vec!["127.0.0.1:1".parse().unwrap()],
        ..BootstrapConfig::default()
    };
    cfg.store.header_capacity = NonZeroU64::new(105_121).unwrap();
    assert!(!cfg.valid());
    cfg.store.header_capacity = NonZeroU64::new(1).unwrap();
    cfg.quorum = 1;
    assert!(!cfg.valid());
}

#[tokio::test]
async fn peer_history_rejects_forgery_incompletion_excess_and_resource_exhaustion() {
    for mode in 0..6 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut stream = peer_handshake(&listener).await;
            assert!(matches!(
                read(&mut stream).await,
                NetworkMessage::GetHeaders(_)
            ));
            let hs = headers();
            let mut batch = match mode {
                0 => hs[1..=15].to_vec(),
                1 => Vec::new(),
                2 => hs[1..=16].to_vec(),
                3 => hs[1..=1].to_vec(),
                4 => hs[1..=15].to_vec(),
                _ => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            };
            if mode == 0 {
                batch[5].nonce ^= 1;
            }
            if mode == 4 {
                batch.swap(5, 6);
            }
            send(&mut stream, NetworkMessage::Headers(batch)).await;
        });
        let hs = headers();
        let anchor = StoreAnchor {
            network: Network::Bitcoin,
            height: 15,
            hash: hs[15].block_hash(),
            height_one_hash: height_one_hash(),
        };
        let mut cfg = config(BootstrapConfig::default().sources, peer, 16);
        cfg.max_header_requests = 1;
        let error = history(&cfg, anchor, 0, hs[0]).await.err().unwrap();
        assert!(
            match mode {
                0 | 4 => matches!(error, BootstrapError::Header(HeaderError::Linkage)),
                1 | 2 => matches!(error, BootstrapError::Reply(_)),
                3 => matches!(error, BootstrapError::Limit("header requests")),
                _ => matches!(error, BootstrapError::Timeout),
            },
            "{error:?}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn quorum_can_tolerate_unavailability_but_never_disagreement() {
    let (a, ah) = source(2021, Fault::None).await;
    let (b, bh) = source(2025, Fault::None).await;
    let (c, ch) = source(2025, Fault::Malformed).await;
    let cfg = config(vec![a, b, c], "127.0.0.1:1".parse().unwrap(), 1);
    let mut http = BootstrapHttp::new(&cfg).unwrap();
    let (anchor, _) = agree(&cfg, &mut http, u64::from(headers()[2015].time))
        .await
        .unwrap();
    assert_eq!(anchor.height, 2015);
    ah.abort();
    bh.abort();
    ch.abort();
}

#[test]
fn one_header_serving_window_preserves_hidden_retarget_context() {
    let hs = headers();
    let anchor = StoreAnchor {
        network: Network::Bitcoin,
        height: 2015,
        hash: hs[2015].block_hash(),
        height_one_hash: height_one_hash(),
    };
    let cfg = StoreConfig {
        header_capacity: NonZeroU64::new(1).unwrap(),
        cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain");
    let store =
        ValidatedHeaderStore::create_with_history(&path, cfg, anchor, &hs[..=2015]).unwrap();
    assert_eq!(store.bounds().unwrap(), (2015, 2015));
    assert!(matches!(
        store.header(2014),
        Err(StoreError::OutsideRetainedHistory)
    ));
    drop(store);
    let mut store = ValidatedHeaderStore::open_pinned(&path, cfg).unwrap();
    store
        .select_suffix(2015, &hs[2016..=2016], u64::from(hs[2016].time))
        .unwrap();
    assert_eq!(store.bounds().unwrap(), (2016, 2016));
}
