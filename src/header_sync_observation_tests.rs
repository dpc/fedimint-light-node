use std::time::Duration;

use bitcoin::Network;
use bitcoin::consensus::deserialize;
use tokio::time::{Instant, advance, timeout};

use super::*;
use crate::peer_pool_config::PeerPoolConfig;
use crate::store_anchor::StoreAnchor;
use crate::store_config::StoreConfig;

fn observed_sync(dir: &std::path::Path, peer: SocketAddr) -> HeaderSync {
    let headers: Vec<Header> = include_bytes!("../tests/data/mainnet-headers-0-2035.raw")
        .as_chunks::<80>()
        .0
        .iter()
        .take(2)
        .map(|bytes| deserialize(bytes).unwrap())
        .collect();
    let store = ValidatedHeaderStore::create(
        dir.join("chain"),
        StoreConfig {
            header_capacity: 4.try_into().unwrap(),
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
    HeaderSync::new_with_mempool(
        store,
        SyncConfig {
            peers: vec![peer],
            peer_pool: PeerPoolConfig::manual_only(),
            handshake_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            // Cancellation occurs during pacing, before any dial.
            retry_delay: Duration::from_secs(1),
            requests_per_visit: 1,
            candidate_headers: 4,
        },
        true,
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn coverage_reservations_survive_cancel_and_skip_useless_deadline_slices() {
    let dir = tempfile::tempdir().unwrap();
    let mut sync = observed_sync(dir.path(), "127.0.0.1:1".parse().unwrap());
    let start = Instant::now();
    let window = sync.coverage_window;
    sync.cover_mempool_until(start + Duration::from_millis(249))
        .await
        .unwrap();
    assert_eq!(sync.coverage_bytes, 8 * 1024 * 1024);
    for expected in [4 * 1024 * 1024, 0] {
        assert!(
            timeout(Duration::from_millis(1), sync.cover_mempool())
                .await
                .is_err()
        );
        assert_eq!(sync.coverage_bytes, expected);
        assert_eq!(sync.coverage_window, window);
        assert_eq!(sync.pool.counts().0, 0);
    }
    // Repeated owner calls and reads cannot reset coverage accounting.
    for _ in 0..10 {
        sync.mempool_snapshot().unwrap();
        sync.cover_mempool().await.unwrap();
        assert_eq!(sync.coverage_bytes, 0);
        assert_eq!(sync.coverage_window, window);
    }
    advance(Duration::from_secs(60)).await;
    assert!(
        timeout(Duration::from_millis(1), sync.cover_mempool())
            .await
            .is_err()
    );
    assert_eq!(sync.coverage_bytes, 4 * 1024 * 1024);
    assert!(sync.coverage_window > window);
    sync.shutdown().await;
}

#[tokio::test]
async fn transaction_membership_expiry_and_reads_preserve_selected_provenance() {
    use bitcoin::blockdata::constants::genesis_block;
    use bitcoin::consensus::serialize;
    use bitcoin::p2p::ServiceFlags;
    use bitcoin::p2p::address::Address;
    use bitcoin::p2p::message::RawNetworkMessage;
    use bitcoin::p2p::message_blockdata::Inventory;
    use bitcoin::p2p::message_network::VersionMessage;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut sync = observed_sync(dir.path(), addr).with_fee_policy(FeePolicy::Observed);
    let block = genesis_block(Network::Bitcoin);
    let mut tx = block.txdata[0].clone();
    tx.input[0].previous_output =
        bitcoin::OutPoint::new(bitcoin::Txid::from_byte_array([3; 32]), 0);
    tx.input[0].witness = bitcoin::Witness::from_slice(&[[1, 2, 3]]);
    let txid = tx.compute_txid();
    sync.refresh_mempool().unwrap();
    sync.mempool.as_mut().unwrap().ingest(0, &block);
    let before = sync.fee_observations().unwrap().unwrap();
    let protected = sync.store.protected_height().unwrap();
    let generation = sync.generation;
    assert!(sync.fee_sats_per_kvb().unwrap().is_none()); // Coinbase-only sample.
    // No current member means no dial, even with a configured reachable endpoint.
    assert!(
        sync.fetch_mempool_transaction(txid)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        timeout(Duration::from_millis(10), listener.accept())
            .await
            .is_err()
    );

    let serving = tokio::spawn(async move {
        let (mut remote, _) = listener.accept().await.unwrap();
        assert!(matches!(
            crate::header_wire::receive(&mut remote, &mut 1000)
                .await
                .unwrap(),
            Message::Version(_)
        ));
        let mut version = VersionMessage::new(
            ServiceFlags::NETWORK | ServiceFlags::WITNESS,
            1,
            Address::new(&addr, ServiceFlags::NONE),
            Address::new(&addr, ServiceFlags::NONE),
            42,
            "/fixture/".into(),
            0,
        );
        version.version = 70016;
        send(&mut remote, NetworkMessage::Version(version))
            .await
            .unwrap();
        send(&mut remote, NetworkMessage::Verack).await.unwrap();
        assert!(matches!(
            crate::header_wire::receive(&mut remote, &mut 1000)
                .await
                .unwrap(),
            Message::Verack
        ));
        for delay in [false, true] {
            let mut frame = [0; 61];
            remote.read_exact(&mut frame).await.unwrap();
            assert_eq!(
                deserialize::<RawNetworkMessage>(&frame)
                    .unwrap()
                    .into_payload(),
                NetworkMessage::GetData(vec![Inventory::WitnessTransaction(txid)])
            );
            if delay {
                advance(Duration::from_millis(100)).await;
            }
            remote
                .write_all(&serialize(&RawNetworkMessage::new(
                    Network::Bitcoin.magic(),
                    NetworkMessage::Tx(tx.clone()),
                )))
                .await
                .unwrap();
        }
        // Keep a healthy socket alive until the owner explicitly shuts it down.
        let _ = remote.read_u8().await;
    });
    let connection = sync
        .pool
        .acquire(
            0,
            Duration::ZERO,
            Duration::from_secs(1),
            &mut 10000,
            &mut 10000,
            true,
        )
        .await
        .unwrap();
    sync.pool.release(connection, false);
    sync.mempool
        .as_mut()
        .unwrap()
        .observe_from(txid, 1, Some(addr), Instant::now());
    let returned = sync.fetch_mempool_transaction(txid).await.unwrap().unwrap();
    assert_eq!(returned.compute_txid(), txid);
    assert!(!returned.input[0].witness.is_empty());
    let after = sync.fee_observations().unwrap().unwrap();
    assert_eq!(before.output_cache_entries, after.output_cache_entries);
    assert_eq!(before.body_coverage_complete, after.body_coverage_complete);
    assert_eq!(sync.store.protected_height().unwrap(), protected);
    assert_eq!(sync.generation, generation);

    // Replace only test membership with a soon-expiring entry; the successful
    // second body must be discarded after its complete network exchange.
    tokio::time::pause();
    // Keep the test runtime runnable: only the peer's explicit advance should
    // move time, not automatic advancement while real loopback I/O is pending.
    let clock_guard = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    sync.mempool
        .as_mut()
        .unwrap()
        .expire(Instant::now() + Duration::from_secs(7200));
    sync.mempool.as_mut().unwrap().observe_from(
        txid,
        1,
        Some(addr),
        Instant::now() - Duration::from_secs(7200) + Duration::from_millis(50),
    );
    assert!(
        sync.fetch_mempool_transaction(txid)
            .await
            .unwrap()
            .is_none()
    );
    clock_guard.abort();
    tokio::time::resume();
    assert!(sync.mempool_snapshot().unwrap().unwrap().is_empty());
    assert!(sync.fee_sats_per_kvb().unwrap().is_none());
    sync.fee_policy = FeePolicy::Fixed {
        sats_per_kvb: 7.try_into().unwrap(),
    };
    assert_eq!(sync.fee_sats_per_kvb().unwrap().unwrap().get(), 7);
    sync.fee_policy = FeePolicy::Unavailable;
    assert!(sync.fee_sats_per_kvb().unwrap().is_none());
    sync.shutdown().await;
    serving.await.unwrap();
}

#[tokio::test]
async fn pinned_client_observed_fractional_fee_and_authoritative_policy_overrides() {
    use crate::guardian_rpc::GuardianRpc;
    use crate::guardian_rpc_config::{GuardianRpcConfig, RpcAuth};
    use bitcoin::blockdata::constants::genesis_block;
    use bitcoincore_rpc::{Auth, Client, RpcApi};

    for mode in ["fresh", "fixed", "unavailable", "stale", "invalid-fixed"] {
        let dir = tempfile::tempdir().unwrap();
        let mut sync = observed_sync(dir.path(), "127.0.0.1:1".parse().unwrap());
        sync.refresh_mempool().unwrap();
        // Private post-admission arithmetic fixture: selected identity is real,
        // synthetic transactions are injected only into the observer, not the
        // production block admission path or persistent selected store.
        let mut sample = genesis_block(Network::Bitcoin);
        sample.txdata[0].output[0].value = bitcoin::Amount::from_sat(1000);
        let mut child = sample.txdata[0].clone();
        child.input[0].previous_output = bitcoin::OutPoint::new(sample.txdata[0].compute_txid(), 0);
        child.output[0].value = bitcoin::Amount::from_sat(999);
        let expected_rate = 1000u64.div_ceil(child.vsize() as u64);
        assert!(expected_rate > 0 && expected_rate < 1000);
        if mode == "invalid-fixed" {
            child.output[0].value = bitcoin::Amount::from_sat(1001);
        }
        sample.txdata.push(child);
        sync.mempool.as_mut().unwrap().ingest(0, &sample);
        let expected = match mode {
            "fixed" | "invalid-fixed" => {
                sync.fee_policy = FeePolicy::Fixed {
                    sats_per_kvb: 777.try_into().unwrap(),
                };
                Some(777)
            }
            "unavailable" => {
                sync.fee_policy = FeePolicy::Unavailable;
                None
            }
            "stale" => {
                sync.fee_policy = FeePolicy::Observed;
                // Move selected tip identity; observation must become unavailable
                // immediately, before any newer body's fee sample is inspected.
                let header = deserialize(
                    &include_bytes!("../tests/data/mainnet-headers-0-2035.raw")[80..160],
                )
                .unwrap();
                sync.store
                    .select_suffix(0, &[header], 2_000_000_000)
                    .unwrap();
                None
            }
            _ => {
                sync.fee_policy = FeePolicy::Observed;
                Some(expected_rate)
            }
        };
        assert_eq!(sync.fee_sats_per_kvb().unwrap().map(|r| r.get()), expected);
        let rpc = GuardianRpc::bind(
            GuardianRpcConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                auth: RpcAuth::NoAuth,
                fake_bitcoind: true,
                allow_submission: false,
            },
            sync,
        )
        .await
        .unwrap();
        let addr = rpc.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(rpc.run(
            async {
                let _ = stopped.await;
            },
            |_, _| {},
        ));
        tokio::task::spawn_blocking(move || {
            let client = Client::new(&format!("http://{addr}"), Auth::None).unwrap();
            assert!(client.get_block_hash(1).is_ok());
            assert!(client.get_block_count().is_ok());
            assert_eq!(
                client.get_blockchain_info().unwrap().chain,
                Network::Bitcoin
            );
            let estimate = client
                .estimate_smart_fee(1, Some(bitcoincore_rpc::json::EstimateMode::Conservative))
                .unwrap();
            assert_eq!(estimate.fee_rate.map(bitcoin::Amount::to_sat), expected);
            assert!(estimate.errors.is_none());
        })
        .await
        .unwrap();
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }
}
