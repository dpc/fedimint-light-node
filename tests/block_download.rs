use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use bitcoin::block::Header;
use bitcoin::blockdata::constants::genesis_block;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::Hash;
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_blockdata::Inventory;
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::{BlockHash, Network};
use fedimint_light_node::block_decode::BlockDecodeLimits;
use fedimint_light_node::block_download_config::BlockDownloadConfig;
use fedimint_light_node::block_download_error::{BlockDownloadError, BlockPeerError};
use fedimint_light_node::header_sync::HeaderSync;
use fedimint_light_node::store_anchor::StoreAnchor;
use fedimint_light_node::store_config::StoreConfig;
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

fn config() -> BlockDownloadConfig {
    BlockDownloadConfig {
        max_attempts: 16,
        total_timeout: Duration::from_secs(3),
        visit_timeout: Duration::from_secs(1),
        max_received_bytes: 8_000_000,
        decode: BlockDecodeLimits {
            max_payload_bytes: 4_000_000,
            max_elements: 100_000,
        },
    }
}

fn service(
    dir: &tempfile::TempDir,
    peers: Vec<SocketAddr>,
    capacity: u64,
    tip: usize,
) -> HeaderSync {
    let headers = headers();
    let mut store = ValidatedHeaderStore::create(
        dir.path().join("chain"),
        StoreConfig {
            header_capacity: NonZeroU64::new(capacity).unwrap(),
            cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
        },
        StoreAnchor {
            network: Network::Bitcoin,
            height: 0,
            hash: headers[0].block_hash(),
            height_one_hash: headers[1].block_hash(),
        },
        &headers[..1],
    )
    .unwrap();
    for chunk in headers[1..=tip].chunks(capacity as usize) {
        store
            .select_suffix(store.bounds().unwrap().1, chunk, 2_000_000_000)
            .unwrap();
    }
    HeaderSync::new(
        store,
        SyncConfig {
            peer_pool: fedimint_light_node::peer_pool_config::PeerPoolConfig::manual_only(),
            peers,
            handshake_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            retry_delay: Duration::from_millis(1),
            requests_per_visit: 1,
            candidate_headers: capacity as usize,
        },
    )
    .unwrap()
}

async fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
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

async fn send(stream: &mut TcpStream, message: NetworkMessage) {
    stream
        .write_all(&serialize(&RawNetworkMessage::new(
            Network::Bitcoin.magic(),
            message,
        )))
        .await
        .unwrap();
}

async fn handshake(listener: TcpListener, services: ServiceFlags) -> TcpStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
        read(&mut stream).await,
        NetworkMessage::Version(_)
    ));
    let version = version(stream.local_addr().unwrap(), services);
    send(&mut stream, NetworkMessage::Version(version)).await;
    send(&mut stream, NetworkMessage::Verack).await;
    assert_eq!(read(&mut stream).await, NetworkMessage::Verack);
    stream
}

fn version(addr: SocketAddr, services: ServiceFlags) -> VersionMessage {
    let address = Address::new(&addr, services);
    VersionMessage::new(
        services,
        0,
        address.clone(),
        address,
        42,
        "/scripted/".into(),
        i32::MAX, // Never establishes freshness or suitability for limited peers.
    )
}

fn full() -> ServiceFlags {
    ServiceFlags::NETWORK | ServiceFlags::WITNESS
}

async fn request(stream: &mut TcpStream, hash: BlockHash) {
    assert_eq!(
        read(stream).await,
        NetworkMessage::GetData(vec![Inventory::WitnessBlock(hash)])
    );
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
async fn retained_legacy_roundtrip_does_not_write_or_protect_state() {
    let dir = tempfile::tempdir().unwrap();
    let (listener, addr) = listener().await;
    let block = genesis_block(Network::Bitcoin);
    let hash = block.block_hash();
    let server = tokio::spawn(async move {
        let mut stream = handshake(listener, full()).await;
        request(&mut stream, hash).await;
        send(&mut stream, NetworkMessage::Ping(42)).await;
        assert_eq!(read(&mut stream).await, NetworkMessage::Pong(42));
        send(&mut stream, NetworkMessage::Block(block)).await;
        closed(&mut stream).await;
    });
    let mut service = service(&dir, vec![addr], 2, 1);
    let before = service.status().unwrap();
    assert_eq!(
        service.fetch_block(0, hash, config()).await.unwrap(),
        genesis_block(Network::Bitcoin)
    );
    assert_eq!(before, service.status().unwrap());
    assert_eq!(service.store().protected_height().unwrap(), 0);
    service.shutdown().await;
    server.await.unwrap();
}

#[tokio::test]
async fn witness_and_full_history_services_required_even_with_high_advertised_height() {
    // Test both locally recent and >288-deep retained genesis requests.
    for tip in [0, 300] {
        for flags in [
            ServiceFlags::NETWORK,
            ServiceFlags::NETWORK_LIMITED | ServiceFlags::WITNESS,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (listener, addr) = listener().await;
            let server = tokio::spawn(async move {
                let mut stream = handshake(listener, flags).await;
                closed(&mut stream).await; // No getdata sent to an unsuitable peer.
            });
            let mut service = service(&dir, vec![addr], 301, tip);
            assert!(matches!(
                service
                    .fetch_block(0, headers()[0].block_hash(), config())
                    .await,
                Err(BlockDownloadError::Unavailable {
                    attempts: 1,
                    last: BlockPeerError::Services
                })
            ));
            server.await.unwrap();
        }
    }
}

#[tokio::test]
async fn wrong_hash_then_full_peer_failover_for_old_retained_block() {
    let dir = tempfile::tempdir().unwrap();
    let (bad, bad_addr) = listener().await;
    let (good, good_addr) = listener().await;
    let hash = headers()[0].block_hash();
    let bad_server = tokio::spawn(async move {
        let mut stream = handshake(bad, full()).await;
        request(&mut stream, hash).await;
        send(
            &mut stream,
            NetworkMessage::Block(genesis_block(Network::Testnet)),
        )
        .await;
        closed(&mut stream).await;
    });
    let good_server = tokio::spawn(async move {
        let mut stream = handshake(good, full()).await;
        request(&mut stream, hash).await;
        send(
            &mut stream,
            NetworkMessage::Block(genesis_block(Network::Bitcoin)),
        )
        .await;
    });
    let mut service = service(&dir, vec![bad_addr, good_addr], 301, 300);
    assert_eq!(
        service
            .fetch_block(0, hash, config())
            .await
            .unwrap()
            .block_hash(),
        hash
    );
    bad_server.await.unwrap();
    good_server.await.unwrap();
}

#[tokio::test]
async fn notfound_is_transient_and_each_peer_is_tried_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let (listener, addr) = listener().await;
    let hash = headers()[0].block_hash();
    let server = tokio::spawn(async move {
        let mut stream = handshake(listener, full()).await;
        request(&mut stream, hash).await;
        send(
            &mut stream,
            NetworkMessage::NotFound(vec![Inventory::Block(hash)]),
        )
        .await;
        closed(&mut stream).await;
    });
    let mut service = service(&dir, vec![addr], 1, 0);
    assert!(matches!(
        service.fetch_block(0, hash, config()).await,
        Err(BlockDownloadError::Unavailable {
            attempts: 1,
            last: BlockPeerError::NotFound
        })
    ));
    assert!(!service.status().unwrap().stopped);
    server.await.unwrap();
}

#[tokio::test]
async fn absolute_visit_and_global_deadlines_close_withholding_peers() {
    for total in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (listener, addr) = listener().await;
        let hash = headers()[0].block_hash();
        let server = tokio::spawn(async move {
            let mut stream = handshake(listener, full()).await;
            request(&mut stream, hash).await;
            closed(&mut stream).await;
        });
        let mut service = service(&dir, vec![addr], 1, 0);
        let mut config = config();
        if total {
            config.total_timeout = Duration::from_millis(100);
        } else {
            config.visit_timeout = Duration::from_millis(100);
        }
        assert!(matches!(
            service.fetch_block(0, hash, config).await,
            Err(BlockDownloadError::Unavailable {
                last: BlockPeerError::Timeout,
                ..
            })
        ));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_releases_the_only_download_slot_and_preserves_selection() {
    let dir = tempfile::tempdir().unwrap();
    let (first, first_addr) = listener().await;
    let (second, second_addr) = listener().await;
    let (requested, received) = oneshot::channel();
    let hash = headers()[0].block_hash();
    let first_server = tokio::spawn(async move {
        let mut stream = handshake(first, full()).await;
        request(&mut stream, hash).await;
        requested.send(()).unwrap();
        closed(&mut stream).await;
    });
    let second_server = tokio::spawn(async move {
        let mut stream = handshake(second, full()).await;
        request(&mut stream, hash).await;
        send(
            &mut stream,
            NetworkMessage::Block(genesis_block(Network::Bitcoin)),
        )
        .await;
    });
    let mut service = service(&dir, vec![first_addr, second_addr], 1, 0);
    let before = service.status().unwrap();
    {
        let pending = service.fetch_block(0, hash, config());
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("unexpected result: {result:?}"),
            _ = received => {}
        }
    }
    assert_eq!(before, service.status().unwrap());
    assert_eq!(
        service
            .fetch_block(0, hash, config())
            .await
            .unwrap()
            .block_hash(),
        hash
    );
    first_server.await.unwrap();
    second_server.await.unwrap();
}

#[tokio::test]
async fn range_and_selection_errors_never_connect_or_attempt_archival_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let (listener, addr) = listener().await;
    let mut service = service(&dir, vec![addr], 1, 1);
    assert!(matches!(
        service
            .fetch_block(0, headers()[0].block_hash(), config())
            .await,
        Err(BlockDownloadError::OutsideRetainedHistory)
    ));
    assert!(matches!(
        service
            .fetch_block(2, BlockHash::all_zeros(), config())
            .await,
        Err(BlockDownloadError::BeyondTip)
    ));
    assert!(matches!(
        service
            .fetch_block(1, headers()[0].block_hash(), config())
            .await,
        Err(BlockDownloadError::NotCanonical)
    ));
    assert!(
        timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn aggregate_budget_includes_failed_peer_handshakes_and_frames() {
    let dir = tempfile::tempdir().unwrap();
    let (first, first_addr) = listener().await;
    let (second, second_addr) = listener().await;
    let hash = headers()[0].block_hash();
    let first_server = tokio::spawn(async move {
        let mut stream = handshake(first, full()).await;
        request(&mut stream, hash).await;
        send(
            &mut stream,
            NetworkMessage::NotFound(vec![Inventory::Block(hash)]),
        )
        .await;
        closed(&mut stream).await;
    });
    let second_server = tokio::spawn(async move {
        let mut stream = handshake(second, full()).await;
        request(&mut stream, hash).await;
        // Header alone suffices for rejecting payload under the remaining budget.
        let frame = serialize(&RawNetworkMessage::new(
            Network::Bitcoin.magic(),
            NetworkMessage::Block(genesis_block(Network::Bitcoin)),
        ));
        stream.write_all(&frame[..24]).await.unwrap();
        closed(&mut stream).await;
    });
    let mut service = service(&dir, vec![first_addr, second_addr], 1, 0);
    let mut config = config();
    config.max_received_bytes = 600; // Enough for either visit, not both.
    assert!(matches!(
        service.fetch_block(0, hash, config).await,
        Err(BlockDownloadError::Unavailable {
            attempts: 2,
            last: BlockPeerError::Peer(SyncError::Protocol(_))
        })
    ));
    first_server.await.unwrap();
    second_server.await.unwrap();
}

#[tokio::test]
async fn malformed_and_partial_envelopes_reserve_bytes_across_peer_failover() {
    for partial in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (first, first_addr) = listener().await;
        let (second, second_addr) = listener().await;
        let hash = headers()[0].block_hash();
        let first_server = tokio::spawn(async move {
            let (mut stream, _) = first.accept().await.unwrap();
            assert!(matches!(
                read(&mut stream).await,
                NetworkMessage::Version(_)
            ));
            // Invalid magic or EOF in the envelope must still reserve 24 bytes.
            stream
                .write_all(&[0; 24][..if partial { 12 } else { 24 }])
                .await
                .unwrap();
        });
        let frame = serialize(&RawNetworkMessage::new(
            Network::Bitcoin.magic(),
            NetworkMessage::Block(genesis_block(Network::Bitcoin)),
        ));
        let mut config = config();
        // Exact budget for the good visit alone; the failed envelope makes it fail.
        config.max_received_bytes = 24
            + frame.len()
            + serialize(&RawNetworkMessage::new(
                Network::Bitcoin.magic(),
                NetworkMessage::Version(version(second_addr, full())),
            ))
            .len();
        let second_server = tokio::spawn(async move {
            let mut stream = handshake(second, full()).await;
            request(&mut stream, hash).await;
            stream.write_all(&frame).await.unwrap();
        });
        let mut service = service(&dir, vec![first_addr, second_addr], 1, 0);
        assert!(matches!(
            service.fetch_block(0, hash, config).await,
            Err(BlockDownloadError::Unavailable {
                attempts: 2,
                last: BlockPeerError::Peer(SyncError::Protocol("frame or exchange byte budget")),
            })
        ));
        first_server.await.unwrap();
        second_server.await.unwrap();
    }
}

#[tokio::test]
async fn corrupt_and_malformed_bodies_fail_without_stopping_headers() {
    for corrupt_checksum in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (listener, addr) = listener().await;
        let hash = headers()[0].block_hash();
        let server = tokio::spawn(async move {
            let mut stream = handshake(listener, full()).await;
            request(&mut stream, hash).await;
            let mut block = genesis_block(Network::Bitcoin);
            if !corrupt_checksum {
                block.txdata.clear();
            }
            let mut frame = serialize(&RawNetworkMessage::new(
                Network::Bitcoin.magic(),
                NetworkMessage::Block(block),
            ));
            if corrupt_checksum {
                frame[20] ^= 1;
            }
            stream.write_all(&frame).await.unwrap();
            closed(&mut stream).await;
        });
        let mut service = service(&dir, vec![addr], 1, 0);
        assert!(matches!(
            service.fetch_block(0, hash, config()).await,
            Err(BlockDownloadError::Unavailable { .. })
        ));
        assert!(!service.status().unwrap().stopped);
        server.await.unwrap();
    }
}
