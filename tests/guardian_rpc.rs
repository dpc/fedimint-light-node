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
use fedimint_light_node::guardian_rpc_config::GuardianRpcConfig;
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
    HeaderSync::new(
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
    )
    .unwrap()
    .with_fee_policy(fee)
}

fn rpc_config(fake: bool, submission: bool) -> GuardianRpcConfig {
    GuardianRpcConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        username: "guardian".into(),
        password: "secret".into(),
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
            config.password.clear();
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
