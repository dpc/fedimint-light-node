use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bitcoin::Network;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_network::VersionMessage;
use rand::SeedableRng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{advance, timeout};

use super::*;
use crate::header_wire::{Message, receive, send};
use crate::peer_address::{discoverable, normalize};

async fn read(stream: &mut TcpStream) -> NetworkMessage {
    let mut envelope = [0; 24];
    stream.read_exact(&mut envelope).await.unwrap();
    let len = u32::from_le_bytes(envelope[16..20].try_into().unwrap()) as usize;
    assert!(len <= 162_003);
    let mut frame = envelope.to_vec();
    frame.resize(24 + len, 0);
    stream.read_exact(&mut frame[24..]).await.unwrap();
    deserialize::<RawNetworkMessage>(&frame)
        .unwrap()
        .into_payload()
}

async fn server(listener: TcpListener) -> TcpStream {
    let (mut stream, addr) = listener.accept().await.unwrap();
    assert!(matches!(
        read(&mut stream).await,
        NetworkMessage::Version(_)
    ));
    let mut version = VersionMessage::new(
        ServiceFlags::NETWORK | ServiceFlags::WITNESS,
        1,
        Address::new(&addr, ServiceFlags::NONE),
        Address::new(&addr, ServiceFlags::NONE),
        5,
        "/test/".into(),
        0,
    );
    version.version = 70016;
    send(&mut stream, NetworkMessage::Version(version))
        .await
        .unwrap();
    send(&mut stream, NetworkMessage::Verack).await.unwrap();
    assert_eq!(read(&mut stream).await, NetworkMessage::Verack);
    stream
}

async fn acquire(pool: &mut PeerPool) -> PeerConnection {
    pool.acquire(
        0,
        Duration::from_millis(1),
        Duration::from_secs(1),
        &mut 2_000_000,
        &mut 2_000_000,
        true,
    )
    .await
    .unwrap()
}

// These fixtures exercise the managed downloader below HeaderSync admission:
// the mined testnet body is not represented as a selected mainnet header.
#[tokio::test]
async fn hostile_body_leases_close_and_recover_within_attempt_policy() {
    use crate::block_decode::BlockDecodeLimits;
    use crate::block_download::download;
    use crate::block_download_config::BlockDownloadConfig;
    use crate::block_download_error::{BlockDownloadError, BlockPeerError};
    use crate::decode_error::DecodeError;
    use crate::frame_error::FrameError;
    use crate::integrity_error::IntegrityError;

    let original: bitcoin::Block =
        deserialize(include_bytes!("../tests/data/testnet-segwit.raw")).unwrap();
    for failure in ["witness", "truncated", "stalled"] {
        for attempts in [1, 2] {
            let bad = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let good = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addresses = [bad.local_addr().unwrap(), good.local_addr().unwrap()];
            let mut corrupt = original.clone();
            corrupt.txdata[0].input[0].witness = bitcoin::Witness::from_slice(&[[1; 32]]);
            assert_eq!(corrupt.block_hash(), original.block_hash());
            assert!(corrupt.check_merkle_root());
            assert!(!corrupt.check_witness_commitment());
            let wire = serialize(&RawNetworkMessage::new(
                Network::Bitcoin.magic(),
                NetworkMessage::Block(corrupt),
            ));
            let bad_task = tokio::spawn(async move {
                let mut stream = server(bad).await;
                assert!(matches!(
                    read(&mut stream).await,
                    NetworkMessage::GetData(_)
                ));
                if failure == "witness" {
                    stream.write_all(&wire).await.unwrap();
                } else {
                    stream.write_all(&wire[..31]).await.unwrap();
                    if failure == "truncated" {
                        stream.shutdown().await.unwrap();
                    }
                }
                // A partial/stalled body must be dropped, never returned to an
                // idle driver. Keep the read half open to observe client closure.
                assert_eq!(
                    timeout(Duration::from_secs(3), stream.read_u8())
                        .await
                        .unwrap()
                        .unwrap_err()
                        .kind(),
                    std::io::ErrorKind::UnexpectedEof
                );
            });
            let body = original.clone();
            let good_task = tokio::spawn(async move {
                let mut stream = server(good).await;
                assert!(matches!(
                    read(&mut stream).await,
                    NetworkMessage::GetData(_)
                ));
                send(&mut stream, NetworkMessage::Block(body))
                    .await
                    .unwrap();
                assert_eq!(
                    timeout(Duration::from_secs(3), stream.read_u8())
                        .await
                        .unwrap()
                        .unwrap_err()
                        .kind(),
                    std::io::ErrorKind::UnexpectedEof
                );
            });
            let mut config = PeerPoolConfig::manual_only();
            config.connections = 1;
            let mut pool = PeerPool::new(&addresses, config).unwrap();
            let mut budget = BlockDownloadConfig {
                max_attempts: attempts,
                total_timeout: Duration::from_secs(2),
                visit_timeout: Duration::from_millis(250),
                max_received_bytes: 20_000,
                decode: BlockDecodeLimits {
                    max_payload_bytes: 5_000,
                    max_elements: 1_000,
                },
            };
            let result = download(
                &mut pool,
                Duration::from_millis(1),
                481_824,
                481_824,
                original.block_hash(),
                budget,
            )
            .await;
            let block = if attempts == 1 {
                let Err(BlockDownloadError::Unavailable { attempts: 1, last }) = result else {
                    panic!("unexpected {failure} result: {result:?}");
                };
                match failure {
                    "witness" => assert!(matches!(
                        last,
                        BlockPeerError::Body(FrameError::Block(DecodeError::Integrity(
                            IntegrityError::WitnessCommitment
                        )))
                    )),
                    "stalled" => assert!(matches!(last, BlockPeerError::Timeout)),
                    _ => assert!(matches!(last, BlockPeerError::Peer(SyncError::Io(_)))),
                }
                bad_task.await.unwrap();
                assert_eq!(pool.counts().0, 0);
                // A failed lease marker is bookkeeping, not a retained socket.
                // The next acquisition must clear it and select the other peer.
                budget.max_attempts = 1;
                download(
                    &mut pool,
                    Duration::from_millis(1),
                    481_824,
                    481_824,
                    original.block_hash(),
                    budget,
                )
                .await
                .unwrap()
            } else {
                bad_task.await.unwrap();
                result.unwrap()
            };
            assert_eq!(block, original);
            assert!(pool.leased.is_none());
            assert!(pool.checking_out.is_none());
            assert!(pool.retiring.is_empty());
            assert_eq!(pool.slots.len(), 1);
            assert_eq!(pool.slots[0].addr, addresses[1]);
            assert!(pool.book.entries[0].useful.is_none());
            assert!(pool.book.entries[1].useful.is_some());
            pool.shutdown().await;
            assert_eq!(pool.counts().0, 0);
            good_task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn core_legacy_alert_threshold_does_not_disconnect_modern_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, addr) = listener.accept().await.unwrap();
        let NetworkMessage::Version(local) = read(&mut stream).await else {
            panic!("expected version");
        };
        let mut version = VersionMessage::new(
            ServiceFlags::NETWORK | ServiceFlags::WITNESS,
            1,
            Address::new(&addr, ServiceFlags::NONE),
            Address::new(&addr, ServiceFlags::NONE),
            5,
            "/core-threshold-fixture/".into(),
            0,
        );
        version.version = 70016;
        send(&mut stream, NetworkMessage::Version(version))
            .await
            .unwrap();
        // Model Core's <=70012 final-alert behavior without copying the obsolete
        // signed payload. Admission must reject its envelope without reading it.
        if local.version <= 70012 {
            let mut envelope = [0; 24];
            envelope[..4].copy_from_slice(&Network::Bitcoin.magic().to_bytes());
            envelope[4..16].copy_from_slice(b"alert\0\0\0\0\0\0\0");
            envelope[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
            stream.write_all(&envelope).await.unwrap();
        }
        assert_eq!(local.version, 70013);
        send(&mut stream, NetworkMessage::FeeFilter(1000))
            .await
            .unwrap();
        send(&mut stream, NetworkMessage::Verack).await.unwrap();
        assert_eq!(read(&mut stream).await, NetworkMessage::Verack);
        send(&mut stream, NetworkMessage::Headers(Vec::new()))
            .await
            .unwrap();
        stream
    });
    let mut pool = PeerPool::new(&[addr], PeerPoolConfig::manual_only()).unwrap();
    let mut peer = acquire(&mut pool).await;
    assert!(matches!(
        receive(&mut peer.stream, &mut 1_000_000).await.unwrap(),
        Message::Headers(headers) if headers.is_empty()
    ));
    pool.release(peer, false);
    let _socket = task.await.unwrap();
    pool.shutdown().await;
}

#[tokio::test]
async fn legacy_alert_still_rejected_before_payload_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut envelope = [0; 24];
        envelope[..4].copy_from_slice(&Network::Bitcoin.magic().to_bytes());
        envelope[4..16].copy_from_slice(b"alert\0\0\0\0\0\0\0");
        envelope[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        stream.write_all(&envelope).await.unwrap();
        stream
    });
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let _socket = task.await.unwrap();
    let mut remaining = usize::MAX;
    assert!(matches!(
        timeout(Duration::from_secs(1), receive(&mut stream, &mut remaining))
            .await
            .unwrap(),
        Err(SyncError::UnsupportedCommand(command)) if command == *b"alert\0\0\0\0\0\0\0"
    ));
}

#[tokio::test]
async fn four_real_sockets_reuse_keepalive_during_lease_and_joined_shutdown() {
    let mut listeners = Vec::new();
    let mut addresses = Vec::new();
    for _ in 0..4 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addresses.push(listener.local_addr().unwrap());
        listeners.push(listener);
    }
    let tasks: Vec<_> = listeners
        .into_iter()
        .map(|l| tokio::spawn(server(l)))
        .collect();
    let mut pool = PeerPool::new(&addresses, PeerPoolConfig::manual_only()).unwrap();
    let mut sockets = Vec::new();
    for task in tasks {
        let peer = acquire(&mut pool).await;
        let addr = peer.addr;
        pool.release(peer, true);
        sockets.push((addr, task.await.unwrap()));
    }
    assert_eq!(pool.counts().0, 4);
    // A body/foreground lease excludes every other foreground operation but not
    // idle control service. No second handshake or fifth socket is needed.
    let peer = acquire(&mut pool).await;
    assert_eq!(pool.counts().0, 3);
    for (addr, socket) in &mut sockets {
        if *addr == peer.addr {
            continue;
        }
        send(socket, NetworkMessage::Ping(71)).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), read(socket)).await.unwrap(),
            NetworkMessage::Pong(71)
        );
    }
    pool.release(peer, false);
    for _ in 0..12 {
        let peer = acquire(&mut pool).await;
        assert!(addresses.contains(&peer.addr));
        pool.release(peer, false);
        assert_eq!(pool.counts().0, 4);
    }
    pool.shutdown().await;
    assert_eq!(pool.counts().0, 0);
    for (_, mut socket) in sockets {
        assert_eq!(
            timeout(Duration::from_secs(1), socket.read_u8())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
}

#[tokio::test]
async fn checked_stable_connections_survive_explorer_rotation() {
    let mut addresses = Vec::new();
    let mut servers = Vec::new();
    for _ in 0..5 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addresses.push(listener.local_addr().unwrap());
        servers.push(tokio::spawn(server(listener)));
    }
    let mut config = PeerPoolConfig::manual_only();
    config.rotation = Duration::from_secs(2);
    let mut pool = PeerPool::new(&addresses, config).unwrap();
    let mut sockets = Vec::new();
    for task in servers.drain(..4) {
        let peer = acquire(&mut pool).await;
        pool.release(peer, true);
        sockets.push(task.await.unwrap());
    }
    pool.slots[3].expires = Instant::now();
    let explorer = acquire(&mut pool).await;
    assert_eq!(explorer.addr, addresses[4]);
    assert_eq!(pool.slots.len(), 3);
    assert!(
        pool.slots
            .iter()
            .all(|s| s.stable && addresses[..3].contains(&s.addr))
    );
    pool.release(explorer, true);
    sockets.push(servers.remove(0).await.unwrap());
    assert_eq!(
        sockets[3].read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn partial_idle_frame_canceled_checkout_is_closed_before_refill() {
    for offset in [1, 4, 12, 23, 24, 27, 31] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(server(listener));
        let mut config = PeerPoolConfig::manual_only();
        config.connections = 1;
        let mut pool = PeerPool::new(&[addr], config).unwrap();
        let peer = acquire(&mut pool).await;
        pool.release(peer, false);
        let mut socket = task.await.unwrap();
        let frame = serialize(&RawNetworkMessage::new(
            Network::Bitcoin.magic(),
            NetworkMessage::Ping(1),
        ));
        socket.write_all(&frame[..offset]).await.unwrap();
        // Let the idle driver begin its non-cancellation-safe frame read.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            timeout(Duration::from_millis(5), acquire(&mut pool))
                .await
                .is_err()
        );
        assert!(pool.checking_out.is_some());
        // Cancellation aborts the driver even without a subsequent pool call.
        assert!(
            timeout(Duration::from_secs(1), socket.read_u8())
                .await
                .unwrap()
                .is_err()
        );
        pool.shutdown().await;
        assert!(pool.retiring.is_empty());
    }
}

#[tokio::test]
async fn continuous_control_frames_do_not_starve_clean_lease() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(server(listener));
    let mut pool = PeerPool::new(&[addr], PeerPoolConfig::manual_only()).unwrap();
    let peer = acquire(&mut pool).await;
    pool.release(peer, false);
    let mut socket = task.await.unwrap();
    for nonce in 0..32 {
        send(&mut socket, NetworkMessage::Pong(nonce))
            .await
            .unwrap();
    }
    let peer = timeout(Duration::from_secs(1), acquire(&mut pool))
        .await
        .unwrap();
    assert_eq!(peer.addr, addr);
    drop(peer);
    pool.shutdown().await;
}

#[tokio::test]
async fn hostile_addr_counts_and_unnegotiated_addrv2_reject_before_payload_reads() {
    for (command, payload_len, prefix) in [
        ("addr", 30_003u32, vec![253, 233, 3]), // count 1001
        ("addr", 30_003, vec![254]),            // huge-width count
        ("addr", 31, vec![253, 1, 0]),          // noncanonical
        ("addrv2", u32::MAX, vec![]),           // never negotiated
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut frame = [0; 24];
        frame[..4].copy_from_slice(&Network::Bitcoin.magic().to_bytes());
        frame[4..4 + command.len()].copy_from_slice(command.as_bytes());
        frame[16..20].copy_from_slice(&payload_len.to_le_bytes());
        socket.write_all(&frame).await.unwrap();
        socket.write_all(&prefix).await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(100),
                receive(&mut client, &mut 2_000_000)
            )
            .await
            .unwrap()
            .is_err()
        );
    }
}

#[tokio::test]
async fn thousand_addresses_have_only_a_bounded_checked_sample() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).await.unwrap();
    let (mut socket, _) = listener.accept().await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;
    let addresses = (0..1000)
        .map(|n| {
            let addr: SocketAddr = format!("11.{}.{}.1:8333", n / 256, n % 256)
                .parse()
                .unwrap();
            (
                now,
                Address::new(&addr, ServiceFlags::NETWORK | ServiceFlags::WITNESS),
            )
        })
        .collect();
    send(&mut socket, NetworkMessage::Addr(addresses))
        .await
        .unwrap();
    let Message::Addresses(sample) = receive(&mut client, &mut 40_000).await.unwrap() else {
        panic!()
    };
    assert_eq!(sample.len(), 32);
    assert!(sample.into_iter().all(discoverable));
}

#[tokio::test(start_paused = true)]
async fn bounded_source_diversity_backoff_and_aging_do_not_trust_gossip() {
    let mut book = PeerBook::new(&[]);
    book.rng = rand::rngs::StdRng::seed_from_u64(17);
    for source in 0..20 {
        for n in 0..1000 {
            book.admit(
                format!("{}.{}.{}.1:8333", 11 + source, n / 256, n % 256)
                    .parse()
                    .unwrap(),
                source,
            );
        }
    }
    assert!(book.entries.len() <= 256);
    for source in 0..20 {
        assert!(book.entries.iter().filter(|e| e.source == source).count() <= 32);
    }
    for entry in &book.entries {
        assert!(
            book.entries
                .iter()
                .filter(|e| group(e.addr) == group(entry.addr))
                .count()
                <= 4
        );
    }
    let addr = book.choose(&[], false).unwrap();
    book.attempted(addr, Duration::ZERO);
    book.admit(addr, 999);
    assert!(book.entries.iter().find(|e| e.addr == addr).unwrap().retry > Instant::now());
    assert_ne!(book.choose(&[addr], false), Some(addr));
    book.completed(addr, true);
    advance(Duration::from_secs(2 * 86400)).await;
    book.age(&[]);
    assert_eq!(book.entries.len(), 1);
    advance(Duration::from_secs(6 * 86400)).await;
    book.age(&[]);
    assert!(book.entries.is_empty());
}

#[test]
fn automatic_targets_cannot_scan_private_or_special_networks() {
    for addr in [
        "127.0.0.1:8333",
        "10.0.0.1:8333",
        "100.64.1.1:8333",
        "169.254.1.1:8333",
        "192.168.1.1:8333",
        "192.0.2.1:8333",
        "198.18.0.1:8333",
        "224.0.0.1:8333",
        "[::1]:8333",
        "[::ffff:127.0.0.1]:8333",
        "[fe80::1]:8333",
        "[fc00::1]:8333",
        "[2001:db8::1]:8333",
        "[2002:a00:1::]:8333",
        "8.8.8.8:80",
    ] {
        assert!(!discoverable(addr.parse().unwrap()), "{addr}");
    }
    assert!(discoverable("8.8.8.8:8333".parse().unwrap()));
    assert!(discoverable("[2606:4700:4700::1111]:8333".parse().unwrap()));
    assert_eq!(
        normalize("[::ffff:8.8.8.8]:8333".parse().unwrap()),
        "8.8.8.8:8333".parse().unwrap()
    );
    let local = "127.0.0.1:1234".parse().unwrap();
    let mut book = PeerBook::new(&[local]);
    assert_eq!(book.choose(&[], false), Some(local));
}

#[tokio::test]
async fn dns_injection_filters_private_results_and_manual_mode_never_resolves() {
    let mut pool = PeerPool::new(&[], PeerPoolConfig::default()).unwrap();
    pool.resolver = Some(tokio::spawn(async {
        Ok(vec![
            "8.8.8.8:8333".parse().unwrap(),
            "127.0.0.1:8333".parse().unwrap(),
        ])
    }));
    pool.discover(Duration::from_secs(1)).await;
    assert_eq!(pool.book.entries.len(), 1);
    assert_eq!(pool.book.entries[0].addr, "8.8.8.8:8333".parse().unwrap());
    let mut pool = PeerPool::new(
        &["127.0.0.1:1".parse().unwrap()],
        PeerPoolConfig::manual_only(),
    )
    .unwrap();
    pool.discover(Duration::from_secs(1)).await;
    assert!(pool.resolver.is_none());
}

#[tokio::test]
async fn failed_exploration_cannot_starve_established_service_or_body_leases() {
    let good = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let good_addr = good.local_addr().unwrap();
    let task = tokio::spawn(server(good));
    let mut stalled = Vec::new();
    let mut addresses = vec![good_addr];
    for _ in 0..8 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addresses.push(listener.local_addr().unwrap());
        stalled.push(listener);
    }
    let mut pool = PeerPool::new(&addresses, PeerPoolConfig::manual_only()).unwrap();
    let peer = acquire(&mut pool).await;
    pool.release(peer, true);
    let _socket = task.await.unwrap();
    for _ in 0..5 {
        assert!(
            pool.acquire(
                0,
                Duration::from_millis(1),
                Duration::from_millis(10),
                &mut 2_000_000,
                &mut 2_000_000,
                true
            )
            .await
            .is_err()
        );
        let peer = acquire(&mut pool).await;
        assert_eq!(peer.addr, good_addr);
        pool.release(peer, true);
    }
    // Body operations do not spend their only attempt on unnecessary exploration.
    let peer = pool
        .acquire(
            0,
            Duration::from_millis(1),
            Duration::from_millis(10),
            &mut 2_000_000,
            &mut 2_000_000,
            false,
        )
        .await
        .unwrap();
    assert_eq!(peer.addr, good_addr);
    pool.release(peer, true);
    pool.shutdown().await;
}

#[tokio::test]
async fn idle_rate_window_survives_repeated_foreground_leases() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(server(listener));
    let mut pool = PeerPool::new(&[addr], PeerPoolConfig::manual_only()).unwrap();
    let peer = acquire(&mut pool).await;
    pool.release(peer, true);
    let mut socket = task.await.unwrap();
    for n in 1..=64 {
        send(&mut socket, NetworkMessage::Ping(n)).await.unwrap();
        assert_eq!(read(&mut socket).await, NetworkMessage::Pong(n));
        let peer = acquire(&mut pool).await;
        assert_eq!(peer.idle_frames, 64 - n as u16);
        assert_eq!(peer.idle_bytes, 256 * 1024 - n as usize * 32);
        pool.release(peer, true);
    }
    send(&mut socket, NetworkMessage::Ping(65)).await.unwrap();
    assert!(
        timeout(Duration::from_secs(1), socket.read_u8())
            .await
            .unwrap()
            .is_err()
    );
    pool.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn old_idle_window_refund_cannot_replenish_new_window() {
    let mut budget = IdleBudget::new();
    let old = budget.reserve(256 * 1024);
    advance(Duration::from_secs(61)).await;
    let new = budget.reserve(1024 * 1024);
    assert_eq!(new.bytes, 1024 * 1024);
    budget.refund(old, 256 * 1024);
    assert_eq!(budget.reserve(1).bytes, 0);
    budget.refund(new, 10);
    assert_eq!(budget.reserve(100).bytes, 10);
}

#[tokio::test]
async fn handshake_success_followed_by_bad_service_preserves_fallback_turn() {
    let mut addresses = Vec::new();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addresses.push(listener.local_addr().unwrap());
        tasks.push(tokio::spawn(server(listener)));
    }
    let mut pool = PeerPool::new(&addresses, PeerPoolConfig::manual_only()).unwrap();
    let good = acquire(&mut pool).await;
    pool.release(good, true);
    let _healthy_socket = tasks.remove(0).await.unwrap();
    for task in tasks {
        let mut explorer = acquire(&mut pool).await;
        assert_ne!(explorer.addr, addresses[0]);
        let mut remote = task.await.unwrap();
        remote.write_all(&[0; 24]).await.unwrap();
        assert!(explorer.receive(&mut 1000, None).await.is_err());
        drop(explorer); // foreground failure must not count as completed service
        let good = acquire(&mut pool).await;
        assert_eq!(good.addr, addresses[0]);
        pool.release(good, true);
    }
    pool.shutdown().await;
}

#[tokio::test]
async fn versioned_cache_is_bounded_disposable_and_never_overwrites_other_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers");
    let mut book = PeerBook::new(&[]);
    let addr = "8.8.8.8:8333".parse().unwrap();
    book.admit(addr, 1);
    book.completed(addr, true);
    crate::peer_cache::save(&path, &book).unwrap();
    let mut loaded = PeerBook::new(&[]);
    crate::peer_cache::load(&path, &mut loaded);
    assert_eq!(loaded.entries.len(), 1);
    std::fs::write(&path, b"not a peer cache").unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(crate::peer_cache::save(&path, &book).is_err());
    crate::peer_cache::load(&path, &mut loaded);
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn idle_budget_is_shared_across_maximum_connection_population() {
    let budget = Arc::new(Mutex::new(IdleBudget::new()));
    // The actual four-driver resource regression is above; this also verifies
    // their shared reservation cannot become four independent 1 MiB budgets.
    let mut remaining = 0;
    for _ in 0..8 {
        // Exercise through public-to-module driver state rather than a body decoder.
        remaining += budget.lock().unwrap().reserve(256 * 1024).bytes;
    }
    assert_eq!(remaining, 1024 * 1024);
}

#[tokio::test]
async fn candidate_selection_is_source_uniform_and_handshake_is_not_usefulness() {
    let mut book = PeerBook::new(&[]);
    book.rng = rand::rngs::StdRng::seed_from_u64(91);
    for n in 1..=32 {
        book.admit(format!("11.{n}.1.1:8333").parse().unwrap(), 1);
    }
    let singleton = "12.1.1.1:8333".parse().unwrap();
    book.admit(singleton, 2);
    let chosen = (0..600)
        .filter(|_| book.choose(&[], true) == Some(singleton))
        .count();
    assert!(
        (200..400).contains(&chosen),
        "a large response must not buy selection weight: {chosen}"
    );
    book.completed(singleton, false);
    assert!(
        book.entries
            .iter()
            .find(|e| e.addr == singleton)
            .unwrap()
            .useful
            .is_none()
    );
    book.completed(singleton, true);
    let success = book
        .entries
        .iter()
        .find(|e| e.addr == singleton)
        .unwrap()
        .useful;
    for source in 0..1000 {
        book.admit(singleton, source);
    }
    assert_eq!(
        book.entries
            .iter()
            .find(|e| e.addr == singleton)
            .unwrap()
            .useful,
        success
    );
}

#[tokio::test]
async fn timed_out_dns_wait_retains_one_job_instead_of_multiplying_resolvers() {
    let mut pool = PeerPool::new(&[], PeerPoolConfig::default()).unwrap();
    let (_send, recv) = tokio::sync::oneshot::channel::<()>();
    pool.resolver = Some(tokio::spawn(async {
        let _ = recv.await;
        Ok(Vec::new())
    }));
    let job = pool.resolver.as_ref().unwrap().id();
    for _ in 0..4 {
        pool.discover(Duration::from_millis(1)).await;
        assert_eq!(pool.resolver.as_ref().unwrap().id(), job);
    }
    pool.shutdown().await;
    assert!(pool.resolver.is_none());
}
