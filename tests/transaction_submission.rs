use std::net::SocketAddr;
use std::time::Duration;

use bitcoin::block::Header;
use bitcoin::blockdata::constants::genesis_block;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::{Network, Transaction, Witness};
use fedimint_light_node::header_sync::HeaderSync;
use fedimint_light_node::store_anchor::StoreAnchor;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::sync_config::SyncConfig;
use fedimint_light_node::transaction_decode::decode_transaction;
use fedimint_light_node::transaction_submission_config::TransactionSubmissionConfig;
use fedimint_light_node::transaction_submission_report::{
    SubmissionPeerError, TransactionSubmissionError,
};
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

fn transaction(witness: bool) -> Transaction {
    let mut tx = genesis_block(Network::Bitcoin).txdata.remove(0);
    if witness {
        tx.input[0].witness = Witness::from_slice(&[b"witness bytes".as_slice(), b"second item"]);
    }
    tx
}

fn config() -> TransactionSubmissionConfig {
    TransactionSubmissionConfig {
        total_timeout: Duration::from_secs(2),
        visit_timeout: Duration::from_millis(250),
        ..TransactionSubmissionConfig::default()
    }
}

fn service(dir: &tempfile::TempDir, peers: Vec<SocketAddr>) -> HeaderSync {
    let headers: Vec<Header> = include_bytes!("data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .take(2)
        .map(|bytes| deserialize(bytes).unwrap())
        .collect();
    let store = ValidatedHeaderStore::create(
        dir.path().join("chain"),
        StoreConfig {
            header_capacity: 8.try_into().unwrap(),
            cache_bytes: (1024 * 1024).try_into().unwrap(),
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
    HeaderSync::new(
        store,
        SyncConfig {
            peers,
            handshake_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            retry_delay: Duration::from_millis(1),
            requests_per_visit: 1,
            candidate_headers: 8,
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
    let mut envelope = [0; 24];
    stream.read_exact(&mut envelope).await.unwrap();
    let len = u32::from_le_bytes(envelope[16..20].try_into().unwrap()) as usize;
    assert!(len <= 400_000);
    let mut frame = envelope.to_vec();
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

fn version(addr: SocketAddr, relay: bool, witness: bool) -> VersionMessage {
    let services = if witness {
        ServiceFlags::NETWORK_LIMITED | ServiceFlags::WITNESS
    } else {
        ServiceFlags::NETWORK
    };
    let mut v = VersionMessage::new(
        services,
        0,
        Address::new(&addr, services),
        Address::new(&addr, services),
        42,
        "/test/".into(),
        0,
    );
    v.relay = relay;
    v.version = 70016;
    v
}

async fn handshake(listener: &TcpListener) -> TcpStream {
    let (mut stream, addr) = listener.accept().await.unwrap();
    let NetworkMessage::Version(local) = read(&mut stream).await else {
        panic!("expected version")
    };
    assert!(!local.relay);
    assert_eq!(local.version, 70012);
    send(
        &mut stream,
        NetworkMessage::Version(version(addr, true, true)),
    )
    .await;
    // Modern peers may offer wtxidrelay. No inventory exchange is needed for a tx.
    send(&mut stream, NetworkMessage::WtxidRelay).await;
    send(&mut stream, NetworkMessage::Ping(42)).await;
    assert_eq!(read(&mut stream).await, NetworkMessage::Verack);
    assert_eq!(read(&mut stream).await, NetworkMessage::Pong(42));
    send(&mut stream, NetworkMessage::Verack).await;
    stream
}

async fn finish(stream: &mut TcpStream) {
    let NetworkMessage::Ping(nonce) = read(stream).await else {
        panic!("expected post-write ping")
    };
    // Irrelevant pong and post-verack chatter must not establish the observation.
    send(stream, NetworkMessage::Pong(nonce.wrapping_add(1))).await;
    send(stream, NetworkMessage::FeeFilter(1)).await;
    send(stream, NetworkMessage::Pong(nonce)).await;
    assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
}

#[tokio::test]
async fn delivers_exact_legacy_and_witness_bytes_and_allows_explicit_replay() {
    let (listener, addr) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![addr]);
    let before = node.status().unwrap();
    let peer = tokio::spawn(async move {
        for witness in [false, true, true] {
            let mut stream = handshake(&listener).await;
            assert_eq!(
                read(&mut stream).await,
                NetworkMessage::Tx(transaction(witness))
            );
            finish(&mut stream).await;
        }
    });
    for witness in [false, true, true] {
        let tx = transaction(witness);
        let report = node.submit_transaction(&tx, config()).await.unwrap();
        assert_eq!(report.submitted(), 1);
        assert_eq!(report.txid, tx.compute_txid());
        assert_eq!(report.wtxid, tx.compute_wtxid());
        assert_eq!(report.attempts.len(), 1);
        assert!(report.attempts[0].payload_write_started);
        assert!(report.attempts[0].post_write.as_ref().unwrap().is_ok());
        assert!(report.sent_bytes > serialize(&tx).len() + 24);
        assert_eq!(node.status().unwrap(), before);
    }
    peer.await.unwrap();
}

#[tokio::test]
async fn refusals_disconnects_and_partial_success_are_reported_per_peer() {
    let (a, aa) = listener().await;
    let (b, ba) = listener().await;
    let (c, ca) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![aa, ba, ca]);
    let peers = tokio::spawn(async move {
        let (mut stream, addr) = a.accept().await.unwrap();
        read(&mut stream).await;
        send(
            &mut stream,
            NetworkMessage::Version(version(addr, false, true)),
        )
        .await;
        assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
        let (mut stream, _) = b.accept().await.unwrap();
        read(&mut stream).await;
        drop(stream);
        let mut stream = handshake(&c).await;
        assert!(matches!(read(&mut stream).await, NetworkMessage::Tx(_)));
    });
    let report = node
        .submit_transaction(&transaction(true), config())
        .await
        .unwrap();
    assert_eq!(report.submitted(), 1);
    assert_eq!(report.attempts.len(), 3);
    assert_eq!(
        report.attempts.iter().map(|a| a.peer).collect::<Vec<_>>(),
        [aa, ba, ca]
    );
    assert!(!report.attempts[0].payload_write_started);
    assert!(!report.attempts[1].payload_write_started);
    peers.await.unwrap();
}

#[tokio::test]
async fn non_witness_peer_is_refused_even_for_legacy_submission() {
    let (listener, addr) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![addr]);
    let peer = tokio::spawn(async move {
        let (mut stream, addr) = listener.accept().await.unwrap();
        read(&mut stream).await;
        send(
            &mut stream,
            NetworkMessage::Version(version(addr, true, false)),
        )
        .await;
        assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
    });
    let report = node
        .submit_transaction(&transaction(false), config())
        .await
        .unwrap();
    assert_eq!(report.submitted(), 0);
    assert!(!report.attempts[0].payload_write_started);
    peer.await.unwrap();
}

#[tokio::test]
async fn old_protocol_version_is_refused_before_payload_write() {
    let (listener, addr) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![addr]);
    let peer = tokio::spawn(async move {
        let (mut stream, addr) = listener.accept().await.unwrap();
        read(&mut stream).await;
        let mut old = version(addr, true, true);
        old.version = 70011;
        send(&mut stream, NetworkMessage::Version(old)).await;
        assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
    });
    let report = node
        .submit_transaction(&transaction(true), config())
        .await
        .unwrap();
    assert_eq!(report.submitted(), 0);
    assert!(!report.attempts[0].payload_write_started);
    peer.await.unwrap();
}

#[tokio::test]
async fn withholding_and_chatter_cannot_extend_absolute_deadlines() {
    let (listener, addr) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![addr]);
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read(&mut stream).await;
        // Never send version/verack; periodically send harmless frames.
        for _ in 0..3 {
            send(&mut stream, NetworkMessage::SendHeaders).await;
            sleep(Duration::from_millis(30)).await;
        }
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    });
    let report = node
        .submit_transaction(&transaction(true), config())
        .await
        .unwrap();
    assert!(matches!(
        report.attempts[0].result,
        Err(SubmissionPeerError::Timeout)
    ));
    assert_eq!(report.submitted(), 0);
    peer.await.unwrap();
}

#[tokio::test]
async fn handshake_message_and_received_byte_budgets_stop_work() {
    for byte_limit in [24, 2_000_000] {
        let (listener, addr) = listener().await;
        let dir = tempfile::tempdir().unwrap();
        let mut node = service(&dir, vec![addr]);
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read(&mut stream).await;
            // One write prevents test-side broken-pipe races with early rejection.
            let frame = serialize(&RawNetworkMessage::new(
                Network::Bitcoin.magic(),
                NetworkMessage::SendHeaders,
            ));
            stream.write_all(&frame.repeat(32)).await.unwrap();
        });
        let report = node
            .submit_transaction(
                &transaction(true),
                TransactionSubmissionConfig {
                    max_received_bytes: byte_limit,
                    ..config()
                },
            )
            .await
            .unwrap();
        assert_eq!(report.submitted(), 0);
        assert!(!report.attempts[0].payload_write_started);
        assert!(report.received_bytes <= byte_limit);
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn sent_budget_reserves_handshake_and_payload_before_writing() {
    let (listener, addr) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![addr]);
    let peer = tokio::spawn(async move {
        let mut stream = handshake(&listener).await;
        assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
    });
    let report = node
        .submit_transaction(
            &transaction(true),
            TransactionSubmissionConfig {
                max_sent_bytes: 250,
                ..config()
            },
        )
        .await
        .unwrap();
    assert_eq!(report.submitted(), 0);
    assert!(matches!(
        report.attempts[0].result,
        Err(SubmissionPeerError::SentBudget)
    ));
    assert!(!report.attempts[0].payload_write_started);
    assert!(report.sent_bytes <= 250);
    peer.await.unwrap();
}

#[tokio::test]
async fn completed_write_survives_missing_pong_and_postwrite_budget_exhaustion() {
    for received_limit in [200, 2_000_000] {
        let (listener, addr) = listener().await;
        let dir = tempfile::tempdir().unwrap();
        let mut node = service(&dir, vec![addr]);
        let peer = tokio::spawn(async move {
            let mut stream = handshake(&listener).await;
            assert!(matches!(read(&mut stream).await, NetworkMessage::Tx(_)));
            assert!(matches!(read(&mut stream).await, NetworkMessage::Ping(_)));
            // Do not answer: the owner must time out/drain-budget out and close.
            assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
        });
        let report = node
            .submit_transaction(
                &transaction(true),
                TransactionSubmissionConfig {
                    max_received_bytes: received_limit,
                    ..config()
                },
            )
            .await
            .unwrap();
        assert_eq!(report.submitted(), 1);
        assert!(report.attempts[0].post_write.as_ref().unwrap().is_err());
        assert!(report.received_bytes <= received_limit);
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn aggregate_budgets_attempt_caps_and_rotation_apply_across_calls() {
    let (a, aa) = listener().await;
    let (b, ba) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![aa, ba]);
    let peers = tokio::spawn(async move {
        for listener in [&a, &b] {
            let mut stream = handshake(listener).await;
            assert!(matches!(read(&mut stream).await, NetworkMessage::Tx(_)));
            finish(&mut stream).await;
        }
    });
    for expected in [aa, ba] {
        let report = node
            .submit_transaction(
                &transaction(true),
                TransactionSubmissionConfig {
                    max_attempts: 1,
                    ..config()
                },
            )
            .await
            .unwrap();
        assert_eq!(report.attempts.len(), 1);
        assert_eq!(report.attempts[0].peer, expected);
        assert_eq!(report.submitted(), 1);
    }
    peers.await.unwrap();
}

#[tokio::test]
async fn total_deadline_includes_pacing_without_opening_socket() {
    let (listener, addr) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![addr]);
    let report = node
        .submit_transaction(
            &transaction(true),
            TransactionSubmissionConfig {
                total_timeout: Duration::from_nanos(1),
                ..config()
            },
        )
        .await
        .unwrap();
    assert_eq!(report.submitted(), 0);
    assert!(matches!(
        report.attempts[0].result,
        Err(SubmissionPeerError::Timeout)
    ));
    assert_eq!(report.sent_bytes, 0);
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancellation_after_delivery_closes_current_socket_and_allows_retry() {
    let (a, aa) = listener().await;
    let (b, ba) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![aa, ba]);
    let (delivered, observed) = oneshot::channel();
    let peers = tokio::spawn(async move {
        let mut first = handshake(&a).await;
        assert!(matches!(read(&mut first).await, NetworkMessage::Tx(_)));
        finish(&mut first).await;
        let (mut second, _) = b.accept().await.unwrap();
        read(&mut second).await;
        delivered.send(()).unwrap();
        assert_eq!(second.read(&mut [0; 1]).await.unwrap(), 0);
        let mut replay = handshake(&a).await;
        assert!(matches!(read(&mut replay).await, NetworkMessage::Tx(_)));
        finish(&mut replay).await;
    });
    let tx = transaction(true);
    {
        let pending = node.submit_transaction(&tx, config());
        tokio::pin!(pending);
        tokio::select! {
            _ = observed => {}
            result = &mut pending => panic!("unexpected completion: {result:?}"),
        }
    }
    let report = node
        .submit_transaction(
            &tx,
            TransactionSubmissionConfig {
                max_attempts: 1,
                ..config()
            },
        )
        .await
        .unwrap();
    assert_eq!(report.submitted(), 1);
    peers.await.unwrap();
}

#[tokio::test]
async fn admission_and_invalid_configuration_do_not_connect_or_rotate() {
    let (listener, addr) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let mut node = service(&dir, vec![addr]);
    let tx = transaction(true);
    for limits in [
        TransactionSubmissionConfig {
            max_transaction_bytes: serialize(&tx).len() - 1,
            ..config()
        },
        TransactionSubmissionConfig {
            max_weight: tx.weight().to_wu() - 1,
            ..config()
        },
        TransactionSubmissionConfig {
            max_attempts: 0,
            ..config()
        },
        TransactionSubmissionConfig {
            total_timeout: Duration::MAX,
            ..config()
        },
    ] {
        assert!(node.submit_transaction(&tx, limits).await.is_err());
    }
    let mut empty = tx.clone();
    empty.input.clear();
    assert!(matches!(
        node.submit_transaction(&empty, config()).await,
        Err(TransactionSubmissionError::Admission)
    ));
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn canonical_raw_admission_rejects_malformed_and_excessive_inputs() {
    for witness in [false, true] {
        let tx = transaction(witness);
        let bytes = serialize(&tx);
        assert_eq!(decode_transaction(&bytes, config()).unwrap(), tx);
        for n in 0..bytes.len() {
            assert!(decode_transaction(&bytes[..n], config()).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_transaction(&trailing, config()).is_err());
        assert!(
            decode_transaction(
                &bytes,
                TransactionSubmissionConfig {
                    max_weight: tx.weight().to_wu() - 1,
                    ..config()
                }
            )
            .is_err()
        );
    }
    let legacy = serialize(&transaction(false));
    let mut noncanonical = legacy[..4].to_vec();
    noncanonical.extend_from_slice(&[253, 1, 0]);
    noncanonical.extend_from_slice(&legacy[5..]);
    assert!(decode_transaction(&noncanonical, config()).is_err());
    let mut excessive_count = legacy[..4].to_vec();
    excessive_count.extend_from_slice(&[255; 9]);
    assert!(decode_transaction(&excessive_count, config()).is_err());
    let mut bad_flag = serialize(&transaction(true));
    bad_flag[5] = 2;
    assert!(decode_transaction(&bad_flag, config()).is_err());
    let mut superfluous = legacy[..4].to_vec();
    superfluous.extend_from_slice(&[0, 1]);
    superfluous.extend_from_slice(&legacy[4..legacy.len() - 4]);
    superfluous.push(0);
    superfluous.extend_from_slice(&legacy[legacy.len() - 4..]);
    assert!(decode_transaction(&superfluous, config()).is_err());
    assert!(decode_transaction(&vec![0; 400_001], config()).is_err());
}
