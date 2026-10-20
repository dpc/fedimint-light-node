#[cfg(target_os = "linux")]
#[path = "support/owner_enospc.rs"]
mod owner_enospc;

use std::net::SocketAddr;
use std::time::Duration;

use bitcoin::Network;
use bitcoin::block::Header;
use bitcoin::blockdata::constants::genesis_block;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_network::VersionMessage;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use fedimint_light_node::fee_policy::FeePolicy;
use fedimint_light_node::guardian_rpc::GuardianRpc;
use fedimint_light_node::guardian_rpc_config::{GuardianRpcConfig, RpcAuth};
use fedimint_light_node::header_sync::HeaderSync;
use fedimint_light_node::peer_pool_config::PeerPoolConfig;
use fedimint_light_node::store_anchor::StoreAnchor;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::sync_config::SyncConfig;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

fn config() -> StoreConfig {
    StoreConfig {
        header_capacity: 4.try_into().unwrap(),
        cache_bytes: (1024 * 1024).try_into().unwrap(),
    }
}

fn service(path: &std::path::Path, peer: SocketAddr, fee: FeePolicy, tip: usize) -> HeaderSync {
    service_observation(path, peer, fee, tip, false)
}

fn service_observation(
    path: &std::path::Path,
    peer: SocketAddr,
    fee: FeePolicy,
    tip: usize,
    mempool: bool,
) -> HeaderSync {
    let headers: Vec<Header> = include_bytes!("data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .take(9)
        .map(|h| deserialize(h).unwrap())
        .collect();
    let mut store = ValidatedHeaderStore::create(
        path,
        config(),
        StoreAnchor {
            network: Network::Bitcoin,
            height: 0,
            hash: headers[0].block_hash(),
            height_one_hash: headers[1].block_hash(),
        },
        &headers[..1],
    )
    .unwrap();
    for chunk in headers[1..=tip].chunks(4) {
        store
            .select_suffix(store.bounds().unwrap().1, chunk, 2_000_000_000)
            .unwrap();
    }
    HeaderSync::new_with_mempool(
        store,
        SyncConfig {
            peers: vec![peer],
            peer_pool: PeerPoolConfig::manual_only(),
            handshake_timeout: Duration::from_millis(100),
            request_timeout: Duration::from_millis(100),
            retry_delay: Duration::from_millis(1),
            requests_per_visit: 1,
            candidate_headers: 4,
        },
        mempool,
    )
    .unwrap()
    .with_fee_policy(fee)
}

#[tokio::test]
async fn mempool_loopback_inventory_rpc_and_autonomous_selected_body_exclusion() {
    use bitcoin::hashes::Hash;
    use bitcoin::p2p::message_blockdata::Inventory;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let allow_body = Arc::new(AtomicBool::new(false));
    let serve_body = allow_body.clone();
    let body_requests = Arc::new(AtomicUsize::new(0));
    let counted_requests = body_requests.clone();
    let block = genesis_block(Network::Bitcoin);
    let confirmed = block.txdata[0].compute_txid();
    let pending = bitcoin::Txid::from_byte_array([7; 32]);
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, addr) = listener.accept().await.unwrap();
            let NetworkMessage::Version(version) = p2p_read(&mut stream).await.unwrap() else {
                panic!("expected version");
            };
            assert!(version.relay);
            assert_eq!(version.version, 70013);
            let address = Address::new(&addr, ServiceFlags::NETWORK | ServiceFlags::WITNESS);
            let mut version = VersionMessage::new(
                ServiceFlags::NETWORK | ServiceFlags::WITNESS,
                0,
                address.clone(),
                address,
                0,
                "/observation-test/".into(),
                0,
            );
            version.version = 70013;
            p2p_send(&mut stream, NetworkMessage::Version(version)).await;
            p2p_send(&mut stream, NetworkMessage::Verack).await;
            assert_eq!(p2p_read(&mut stream).await.unwrap(), NetworkMessage::Verack);
            while let Ok(message) = p2p_read(&mut stream).await {
                match message {
                    NetworkMessage::GetHeaders(_) => {
                        // Inventory is received during a foreground lease, not just idle.
                        p2p_send(
                            &mut stream,
                            NetworkMessage::Inv(vec![
                                Inventory::Transaction(confirmed),
                                Inventory::Transaction(pending),
                            ]),
                        )
                        .await;
                        p2p_send(&mut stream, NetworkMessage::Headers(vec![])).await;
                        // An idle keepalive guarantees a clean checked inventory boundary.
                        p2p_send(
                            &mut stream,
                            NetworkMessage::Inv(vec![Inventory::Transaction(pending)]),
                        )
                        .await;
                    }
                    NetworkMessage::GetData(items) => {
                        counted_requests.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(items, vec![Inventory::WitnessBlock(block.block_hash())]);
                        if serve_body.load(Ordering::SeqCst) {
                            // A healthy body slower than the disabled-mode one-second
                            // header interval must still complete through the RPC owner.
                            tokio::time::sleep(Duration::from_millis(1500)).await;
                            p2p_send(&mut stream, NetworkMessage::Block(block.clone())).await;
                        } else {
                            p2p_send(&mut stream, NetworkMessage::NotFound(items)).await;
                        }
                    }
                    NetworkMessage::Ping(nonce) => {
                        p2p_send(&mut stream, NetworkMessage::Pong(nonce)).await
                    }
                    NetworkMessage::Pong(_) => {}
                    other => panic!("unexpected message {other:?}"),
                }
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut sync = service_observation(
        &dir.path().join("chain"),
        address,
        FeePolicy::Unavailable,
        0,
        true,
    );
    assert!(sync.step(2_000_000_000).await.result.is_ok());
    assert_eq!(sync.mempool_snapshot().unwrap().unwrap().len(), 2);
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
    let (stop, stopped) = oneshot::channel();
    let (visits, mut visit_times) = tokio::sync::mpsc::channel(8);
    let task = tokio::spawn(rpc.run(
        async {
            let _ = stopped.await;
        },
        move |_, _| {
            let _ = visits.try_send(tokio::time::Instant::now());
        },
    ));
    async fn query(addr: SocketAddr, params: Value) -> Value {
        query_method(addr, "getrawmempool", params).await
    }
    async fn query_method(addr: SocketAddr, method: &str, params: Value) -> Value {
        let body = json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}).to_string();
        let response = wire(addr, format!(
            "POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()
        ).as_bytes()).await;
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    }
    let initial = query(addr, json!([])).await;
    let fee_warmup = query_method(addr, "getfeeobservations", json!([])).await;
    assert!(fee_warmup["result"]["block_hash"].is_null());
    assert_eq!(fee_warmup["result"]["stale"], true);
    assert!(
        initial["result"]
            .as_array()
            .unwrap()
            .contains(&json!(confirmed.to_string()))
    );
    allow_body.store(true, Ordering::SeqCst);
    // Leave a full idle slice. Polling itself deliberately cancels coverage, and
    // an initial NotFound can defer the next body attempt until the next header.
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert_eq!(
        query(addr, json!([false])).await["result"],
        json!([pending.to_string()])
    );
    // This is a real admitted selected mainnet body over loopback P2P, not a
    // synthetic header bypass. Genesis has no non-coinbase rates to resolve.
    let count = body_requests.load(Ordering::SeqCst);
    for _ in 0..3 {
        let fee = query_method(addr, "getfeeobservations", json!([])).await;
        assert_eq!(
            fee["result"]["block_hash"],
            genesis_block(Network::Bitcoin).block_hash().to_string()
        );
        assert_eq!(fee["result"]["body_coverage_complete"], 1);
        assert_eq!(fee["result"]["sampled_tx_count"], 0);
        assert_eq!(fee["result"]["output_cache_entries"], 1);
        assert!(fee["result"]["subset_p50_sat_per_vb"].is_null());
        for metadata in ["source", "trust", "warning"] {
            assert!(fee["result"].get(metadata).is_none(), "{fee}");
        }
    }
    assert_eq!(body_requests.load(Ordering::SeqCst), count);
    let fixed_policy = query_method(addr, "estimatesmartfee", json!([1, "CONSERVATIVE"])).await;
    assert!(fixed_policy["result"]["feerate"].is_null());
    assert!(fixed_policy["result"]["errors"].is_array());
    assert_eq!(
        query_method(addr, "getfeeobservations", json!([false])).await["error"]["code"],
        -32602
    );
    let first = visit_times.recv().await.unwrap();
    let second = visit_times.recv().await.unwrap();
    assert!(second.duration_since(first) >= Duration::from_secs(5));
    assert!(second.duration_since(first) < Duration::from_secs(6));
    for params in [
        json!([true]),
        json!([false, false]),
        json!([0]),
        json!([null]),
    ] {
        assert_eq!(query(addr, params).await["error"]["code"], -32602);
    }
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    server.abort();
    let _ = server.await;
}

fn rpc_config(fake: bool, submission: bool) -> GuardianRpcConfig {
    GuardianRpcConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        auth: RpcAuth::Basic {
            username: "guardian".into(),
            password: "secret".into(),
        },
        fake_bitcoind: fake,
        allow_submission: submission,
    }
}

async fn start(
    sync: HeaderSync,
    fake: bool,
    submission: bool,
) -> (
    SocketAddr,
    oneshot::Sender<()>,
    JoinHandle<std::io::Result<()>>,
) {
    let rpc = GuardianRpc::bind(rpc_config(fake, submission), sync)
        .await
        .unwrap();
    let addr = rpc.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(rpc.run(
        async {
            let _ = stopped.await;
        },
        |_, _| {},
    ));
    (addr, stop, task)
}

fn client(addr: SocketAddr) -> Client {
    Client::new(
        &format!("http://{addr}"),
        Auth::UserPass("guardian".into(), "secret".into()),
    )
    .unwrap()
}

async fn wire(addr: SocketAddr, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(response).unwrap()
}

async fn json_wire(addr: SocketAddr, value: Value) -> Value {
    let body = value.to_string();
    let response = wire(addr, format!("POST / HTTP/1.1\r\nAuthorization: Basic Z3VhcmRpYW46c2VjcmV0\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await;
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
}

#[tokio::test]
async fn pinned_guardian_client_status_fee_height_identity_and_no_protection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain");
    let sync = service(
        &path,
        "127.0.0.1:1".parse().unwrap(),
        FeePolicy::Fixed {
            sats_per_kvb: 1001.try_into().unwrap(),
        },
        8,
    );
    let expected = sync.status().unwrap();
    let identity = sync.chain_id();
    let (addr, stop, task) = start(sync, true, false).await;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}).to_string();
    let response = wire(addr, format!(
        "POST / HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Basic Z3VhcmRpYW46c2VjcmV0\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()
    ).as_bytes()).await;
    let value: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(value["result"]["warnings"], json!([]));
    tokio::task::spawn_blocking(move || {
        let rpc = client(addr);
        assert_eq!(rpc.get_block_count().unwrap(), 8); // Fedimint adds one.
        assert_eq!(rpc.get_block_hash(1).unwrap(), identity); // Outside retained range.
        assert!(
            rpc.get_block_hash(2)
                .unwrap_err()
                .to_string()
                .contains("-32010")
        );
        assert_eq!(rpc.get_block_hash(8).unwrap(), expected.hash);
        let info = rpc.get_blockchain_info().unwrap(); // Also calls getnetworkinfo.
        assert_eq!(info.blocks, 8);
        assert_eq!(info.headers, 8);
        assert_eq!(info.best_block_hash, expected.hash);
        assert_eq!(info.chain, Network::Bitcoin);
        assert_eq!(info.chain_work, vec![0; 32]);
        assert!(!info.initial_block_download);
        assert_eq!(
            rpc.estimate_smart_fee(1, Some(bitcoincore_rpc::json::EstimateMode::Conservative))
                .unwrap()
                .fee_rate
                .unwrap()
                .to_sat(),
            1001
        );
        assert!(
            rpc.call::<Value>("getblock", &[json!(identity.to_string()), json!(0)])
                .unwrap_err()
                .to_string()
                .contains("-32011")
        );
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    let store = ValidatedHeaderStore::open_pinned(path, config()).unwrap();
    assert_eq!(store.protected_height().unwrap(), 0);
    assert_eq!(store.bounds().unwrap(), (5, 8));
}

#[tokio::test]
async fn default_off_and_strict_json_dialect() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, stop, task) = start(
        service(
            &dir.path().join("chain"),
            "127.0.0.1:1".parse().unwrap(),
            FeePolicy::Unavailable,
            2,
        ),
        false,
        false,
    )
    .await;
    tokio::task::spawn_blocking(move || {
        let rpc = client(addr);
        assert!(
            rpc.call::<Value>("getrawmempool", &[])
                .unwrap_err()
                .to_string()
                .contains("--mempool")
        );
        assert!(
            rpc.call::<Value>("getfeeobservations", &[])
                .unwrap_err()
                .to_string()
                .contains("--mempool")
        );
        assert!(
            rpc.get_blockchain_info()
                .unwrap_err()
                .to_string()
                .contains("--fake-bitcoind")
        );
        let fee = rpc
            .estimate_smart_fee(1, Some(bitcoincore_rpc::json::EstimateMode::Conservative))
            .unwrap();
        assert!(fee.fee_rate.is_none());
        assert!(fee.errors.is_some());
        assert!(
            rpc.call::<Value>("sendrawtransaction", &[json!("00")])
                .unwrap_err()
                .to_string()
                .contains("--allow-rpc-submission")
        );
    })
    .await
    .unwrap();
    for (method, params, code) in [
        ("getblock", json!(["00"]), -32602),
        ("getblock", json!(["00", 1]), -32602),
        ("getblockhash", json!([-1]), -32602),
        ("getblockcount", json!({"height":1}), -32602),
        ("getblockcount", json!([1]), -32602),
        ("estimatesmartfee", json!([2, "CONSERVATIVE"]), -32602),
        ("stop", json!([]), -32601),
    ] {
        let response = json_wire(
            addr,
            json!({"jsonrpc":"2.0","id":7,"method":method,"params":params}),
        )
        .await;
        assert_eq!(response["error"]["code"], code, "{response}");
        assert!(response.get("result").is_none());
        assert_eq!(response["id"], 7);
    }
    let batch = json_wire(addr, json!([{"id":1,"method":"getblockcount"}])).await;
    assert_eq!(batch["error"]["code"], -32600);
    let legacy = json_wire(
        addr,
        json!({"jsonrpc":"1.0","id":"cli","method":"getblockcount","params":[]}),
    )
    .await;
    assert_eq!(legacy["result"], 2);
    assert!(legacy["error"].is_null());
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn disabled_observation_keeps_the_existing_owner_header_cadence() {
    let dir = tempfile::tempdir().unwrap();
    // A local silent listener exercises bounded failed visits without external traffic.
    let peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sync = service(
        &dir.path().join("chain"),
        peer.local_addr().unwrap(),
        FeePolicy::Unavailable,
        0,
    );
    let rpc = GuardianRpc::bind(rpc_config(false, false), sync)
        .await
        .unwrap();
    let (stop, stopped) = oneshot::channel();
    let (send, mut visits) = tokio::sync::mpsc::channel(2);
    let task = tokio::spawn(rpc.run(
        async {
            let _ = stopped.await;
        },
        move |_, _| {
            let _ = send.try_send(tokio::time::Instant::now());
        },
    ));
    tokio::time::timeout(Duration::from_secs(3), async {
        let first = visits.recv().await.unwrap();
        let second = visits.recv().await.unwrap();
        assert!(second.duration_since(first) >= Duration::from_secs(1));
        assert!(second.duration_since(first) < Duration::from_secs(2));
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn authentication_framing_limits_and_shutdown_with_partial_connections() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, stop, task) = start(
        service(
            &dir.path().join("chain"),
            "127.0.0.1:1".parse().unwrap(),
            FeePolicy::Unavailable,
            2,
        ),
        false,
        false,
    )
    .await;
    for (headers, status) in [
        ("Content-Length: 0\r\n", "401"),
        ("Authorization: Basic bad\r\nContent-Length: 0\r\n", "401"),
        (
            "Authorization: Basic Z3VhcmRpYW46c2VjcmV0\r\nContent-Length: 999999999\r\n",
            "413",
        ),
        (
            "Authorization: Basic Z3VhcmRpYW46c2VjcmV0\r\nContent-Length: 0\r\nContent-Length: 0\r\n",
            "400",
        ),
        (
            "Authorization: Basic Z3VhcmRpYW46c2VjcmV0\r\nTransfer-Encoding: chunked\r\n",
            "400",
        ),
        (
            "Authorization: Basic Z3VhcmRpYW46c2VjcmV0\r\nContent-Encoding: gzip\r\n",
            "400",
        ),
    ] {
        let response = wire(addr, format!("POST / HTTP/1.1\r\n{headers}\r\n").as_bytes()).await;
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "{response}"
        );
    }
    let oversized = format!("POST / HTTP/1.1\r\nX-Test: {}\r\n\r\n", "x".repeat(4096));
    // A peer may reset rather than drain the rejected oversized request.
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(oversized.as_bytes()).await.unwrap();
    let mut response = [0; 256];
    let n = stream.read(&mut response).await.unwrap();
    assert!(String::from_utf8_lossy(&response[..n]).starts_with("HTTP/1.1 431"));
    let mut partial = Vec::new();
    for _ in 0..8 {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"POST /").await.unwrap();
        partial.push(stream);
    }
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn unauthenticated_loopback_reads_reject_submission_and_browser_requests() {
    for bind in ["127.0.0.1:0", "[::1]:0"] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = rpc_config(false, false);
        config.auth = RpcAuth::NoAuth;
        config.bind = bind.parse().unwrap();
        let rpc = GuardianRpc::bind(
            config,
            service(
                &dir.path().join("chain"),
                "127.0.0.1:1".parse().unwrap(),
                FeePolicy::Unavailable,
                2,
            ),
        )
        .await
        .unwrap();
        let addr = rpc.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(rpc.run(
            async {
                let _ = stopped.await;
            },
            |_, _| {},
        ));
        let mut hosts = vec![
            addr.to_string(),
            addr.ip().to_string(),
            "localhost".to_string(),
            format!("localhost:{}", addr.port()),
        ];
        if addr.is_ipv6() {
            hosts.push(format!("[{}]", addr.ip()));
        }
        for host in hosts {
            let body = r#"{"method":"getblockcount","params":[],"id":1,"jsonrpc":"2.0"}"#;
            let response = wire(addr, format!(
                "POST / HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len(),
            ).as_bytes()).await;
            assert!(response.contains("\"result\":2"), "{response}");
            assert!(!response.contains("WWW-Authenticate"));
        }
        for auth in ["", "Authorization: Basic dummy\r\n"] {
            for (method, expected) in [
                ("getblockcount", "\"result\":2"),
                ("sendrawtransaction", "-32601"),
                ("getblockchaininfo", "-32601"),
            ] {
                let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":[]}).to_string();
                let response = wire(addr, format!(
                    "POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\n\r\n{body}", body.len(),
                ).as_bytes()).await;
                assert!(response.starts_with("HTTP/1.1 200"), "{response}");
                assert!(response.contains(expected), "{response}");
            }
        }
        // Native pinned client works both without auth and with dummy client credentials.
        tokio::task::spawn_blocking(move || {
            for auth in [Auth::None, Auth::UserPass("demo".into(), "demo".into())] {
                let client = Client::new(&format!("http://{addr}"), auth).unwrap();
                assert_eq!(client.get_block_count().unwrap(), 2);
            }
        })
        .await
        .unwrap();
        for (extra, status) in [
            ("Origin: https://attacker.example\r\n", "403"),
            ("Origin: null\r\n", "403"),
            ("Host: attacker.example\r\n", "400"),
            ("Content-Type: text/plain\r\n", "400"),
            ("Authorization: a\r\nAuthorization: b\r\n", "400"),
            ("Bad Header: x\r\n", "400"),
            ("Transfer-Encoding: chunked\r\n", "400"),
            ("Content-Encoding: gzip\r\n", "400"),
            ("Content-Length: 0\r\n", "400"),
        ] {
            let response = wire(addr, format!("POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n{extra}Content-Length: 0\r\n\r\n").as_bytes()).await;
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}")),
                "{response}"
            );
        }
        for (host, content_type, status) in [
            ("attacker.example".to_string(), "application/json", "403"),
            (addr.to_string(), "text/plain", "415"),
            (addr.to_string(), "application/x-www-form-urlencoded", "415"),
            (
                format!("localhost:{}", addr.port().wrapping_add(1)),
                "application/json",
                "403",
            ),
        ] {
            let response = wire(addr, format!("POST / HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\nContent-Length: 0\r\n\r\n").as_bytes()).await;
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}")),
                "{response}"
            );
        }
        for (headers, status) in [
            ("Content-Type: application/json\r\n".to_string(), "403"),
            (format!("Host: {addr}\r\n"), "415"),
        ] {
            let response = wire(
                addr,
                format!("POST / HTTP/1.1\r\n{headers}Content-Length: 0\r\n\r\n").as_bytes(),
            )
            .await;
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}")),
                "{response}"
            );
        }
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn unauthenticated_library_configuration_is_loopback_and_read_only() {
    for (bind, submission) in [
        ("0.0.0.0:0", false),
        ("[::]:0", false),
        ("192.0.2.1:0", false),
        ("127.0.0.1:0", true),
        ("[::1]:0", true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = rpc_config(false, submission);
        config.auth = RpcAuth::NoAuth;
        config.bind = bind.parse().unwrap();
        assert!(
            GuardianRpc::bind(
                config,
                service(
                    &dir.path().join("chain"),
                    "127.0.0.1:1".parse().unwrap(),
                    FeePolicy::Unavailable,
                    2
                ),
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn reject_public_bind_empty_credentials_and_lossy_fee_before_listening() {
    for variant in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let fee = if variant == 2 {
            FeePolicy::Fixed {
                sats_per_kvb: u64::MAX.try_into().unwrap(),
            }
        } else {
            FeePolicy::Unavailable
        };
        let sync = service(
            &dir.path().join("chain"),
            "127.0.0.1:1".parse().unwrap(),
            fee,
            2,
        );
        let mut config = rpc_config(false, false);
        if variant == 0 {
            config.bind = "0.0.0.0:0".parse().unwrap();
        }
        if variant == 1 {
            config.auth = RpcAuth::Basic {
                username: "guardian".into(),
                password: String::new(),
            };
        }
        assert!(GuardianRpc::bind(config, sync).await.is_err());
    }
}

async fn p2p_read(stream: &mut TcpStream) -> std::io::Result<NetworkMessage> {
    let mut head = [0; 24];
    stream.read_exact(&mut head).await?;
    let len = u32::from_le_bytes(head[16..20].try_into().unwrap()) as usize;
    assert!(len < 2_000_000);
    let mut bytes = head.to_vec();
    bytes.resize(24 + len, 0);
    stream.read_exact(&mut bytes[24..]).await?;
    Ok(deserialize::<RawNetworkMessage>(&bytes)
        .unwrap()
        .into_payload())
}

async fn p2p_send(stream: &mut TcpStream, message: NetworkMessage) {
    stream
        .write_all(&serialize(&RawNetworkMessage::new(
            Network::Bitcoin.magic(),
            message,
        )))
        .await
        .unwrap();
}

async fn peer(stream: &mut TcpStream, stalled: Option<tokio::sync::mpsc::Sender<&'static str>>) {
    assert!(matches!(
        p2p_read(stream).await.unwrap(),
        NetworkMessage::Version(_)
    ));
    let flags = ServiceFlags::NETWORK | ServiceFlags::WITNESS;
    let address = Address::new(&stream.local_addr().unwrap(), flags);
    let mut version = VersionMessage::new(
        flags,
        0,
        address.clone(),
        address,
        42,
        "/rpc-test/".into(),
        2,
    );
    version.relay = true;
    version.version = 70016;
    p2p_send(stream, NetworkMessage::Version(version)).await;
    p2p_send(stream, NetworkMessage::Verack).await;
    while let Ok(message) = p2p_read(stream).await {
        match message {
            NetworkMessage::GetHeaders(_) => {
                p2p_send(stream, NetworkMessage::Headers(vec![])).await
            }
            NetworkMessage::GetData(_) => {
                if let Some(events) = &stalled {
                    events.send("block").await.unwrap();
                    let mut byte = [0];
                    assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
                    events.send("closed").await.unwrap();
                    return;
                }
                p2p_send(
                    stream,
                    NetworkMessage::Block(genesis_block(Network::Bitcoin)),
                )
                .await
            }
            NetworkMessage::Ping(nonce) => p2p_send(stream, NetworkMessage::Pong(nonce)).await,
            _ => {}
        }
    }
}

#[tokio::test]
async fn pinned_client_raw_block_and_submission_only_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            peer(&mut stream, None).await;
        }
    });
    let (addr, stop, task) = start(
        service(&dir.path().join("chain"), addr, FeePolicy::Unavailable, 2),
        true,
        true,
    )
    .await;
    tokio::task::spawn_blocking(move || {
        let rpc = client(addr);
        let expected = genesis_block(Network::Bitcoin);
        assert_eq!(rpc.get_block(&expected.block_hash()).unwrap(), expected);
        // A local scripted peer observes bytes; this is not a real broadcast or valid spend.
        let tx = &expected.txdata[0];
        assert_eq!(rpc.send_raw_transaction(tx).unwrap(), tx.compute_txid());
        assert_eq!(rpc.send_raw_transaction(tx).unwrap(), tx.compute_txid()); // Explicit duplicate.
        assert!(
            rpc.call::<Value>("sendrawtransaction", &[json!("00")])
                .unwrap_err()
                .to_string()
                .contains("-32602")
        );
        assert!(
            rpc.call::<Value>("sendrawtransaction", &[json!("00"), json!(0)])
                .unwrap_err()
                .to_string()
                .contains("-32602")
        );
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    server.abort();
    let _ = server.await;
    assert_eq!(
        ValidatedHeaderStore::open_pinned(dir.path().join("chain"), config())
            .unwrap()
            .protected_height()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn owner_keeps_sync_and_cheap_reads_live_and_cancels_active_peer_lease() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    for disconnect in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let (events, mut observed) = tokio::sync::mpsc::channel(2);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            peer(&mut stream, Some(events)).await;
        });
        let sync = service(
            &dir.path().join("chain"),
            peer_addr,
            FeePolicy::Unavailable,
            2,
        );
        let rpc = GuardianRpc::bind(rpc_config(false, false), sync)
            .await
            .unwrap();
        let addr = rpc.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let visits = Arc::new(AtomicUsize::new(0));
        let count = visits.clone();
        let task = tokio::spawn(rpc.run(
            async {
                let _ = stopped.await;
            },
            move |_, _| {
                count.fetch_add(1, Ordering::Relaxed);
            },
        ));
        let mut block_client = TcpStream::connect(addr).await.unwrap();
        let body = json!({"jsonrpc":"2.0","id":1,"method":"getblock","params":[genesis_block(Network::Bitcoin).block_hash().to_string(),0]}).to_string();
        block_client.write_all(format!("POST / HTTP/1.1\r\nAuthorization: Basic Z3VhcmRpYW46c2VjcmV0\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), observed.recv())
                .await
                .unwrap(),
            Some("block")
        );
        let before = visits.load(Ordering::Relaxed);
        let cheap = tokio::spawn(json_wire(
            addr,
            json!({"jsonrpc":"2.0","id":2,"method":"getblockcount","params":[]}),
        ));
        if disconnect {
            drop(block_client);
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), observed.recv())
                    .await
                    .unwrap(),
                Some("closed")
            );
        } else {
            // Keep the HTTP request alive while the block-serving peer stalls.
            let mut response = String::new();
            tokio::time::timeout(
                Duration::from_secs(11),
                block_client.read_to_string(&mut response),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(response.contains("-32012"));
            assert_eq!(observed.recv().await, Some("closed"));
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(11), cheap)
                .await
                .unwrap()
                .unwrap()["result"],
            2
        );
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(visits.load(Ordering::Relaxed) > before);
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
        server.await.unwrap();
    }
}
