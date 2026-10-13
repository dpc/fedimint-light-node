//! Finite offline end-to-end pressure measurement with externally pinned real data.

#[path = "readiness_body_recovery_tests.rs"]
mod body_recovery;

use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use bitcoin::block::Header;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{Hash, sha256, sha256d};
use bitcoin::hex::DisplayHex;
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::address::Address;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::{Block, Network};
use redb::{Database, ReadableDatabase, ReadableTableMetadata, TableDefinition};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};

use crate::guardian_rpc::GuardianRpc;
use crate::guardian_rpc_config::GuardianRpcConfig;
use crate::header_sync::HeaderSync;
use crate::peer_pool_config::PeerPoolConfig;
use crate::readiness_observer::{self as observer, Counter, State};
use crate::store_anchor::StoreAnchor;
use crate::store_config::StoreConfig;
use crate::sync_config::SyncConfig;
use crate::validated_header_store::ValidatedHeaderStore;

/// Shared fixture body and bounded network hold, never an injected RPC result.
struct Fixture {
    /// One shared copy; all peers transmit the same mined canonical body.
    raw: Vec<u8>,
    /// Number of actual P2P getdata requests received.
    requests: AtomicUsize,
    /// Hold response at a real socket boundary to exercise normal queue pressure.
    hold: AtomicBool,
    /// Wake held peer responses without extending production deadlines.
    release: Notify,
}

fn bounded_file(path: impl AsRef<Path>, limit: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= limit);
    bytes
}

fn settings() -> StoreConfig {
    StoreConfig {
        header_capacity: 52_560.try_into().unwrap(),
        cache_bytes: (8 * 1024 * 1024).try_into().unwrap(),
    }
}

fn replay(path: &Path) -> (ValidatedHeaderStore, StoreAnchor) {
    let raw = bounded_file(
        std::env::var("READINESS_HEADERS").expect("READINESS_HEADERS"),
        12_775_680,
    );
    assert_eq!(raw.len(), 12_775_680);
    assert_eq!(
        sha256::Hash::hash(&raw).to_string(),
        "0d393a3042f676fdae5e2e6772622bae0b69973f0a19972feb660f04baebaafa"
    );
    let headers: Vec<Header> = raw
        .chunks_exact(80)
        .map(|bytes| deserialize(bytes).unwrap())
        .collect();
    drop(raw);
    let anchor = StoreAnchor {
        network: Network::Bitcoin,
        height: 812447,
        hash: headers[2015].block_hash(),
        height_one_hash: deserialize::<Header>(
            &include_bytes!("../tests/data/mainnet-headers-0-2035.raw")[80..160],
        )
        .unwrap()
        .block_hash(),
    };
    let mut store =
        ValidatedHeaderStore::create(path, settings(), anchor, &headers[..2016]).unwrap();
    let mut tip = anchor.height;
    for batch in headers[2016..].chunks(144) {
        store.select_suffix(tip, batch, u64::MAX).unwrap();
        tip += batch.len() as u64;
    }
    assert_eq!(store.bounds().unwrap(), (917568, 970127));
    assert_eq!(store.tip_context().unwrap().height(), 970127);
    (store, anchor)
}

fn inspect_rows(path: &Path) {
    let db = Database::builder()
        .set_cache_size(1024 * 1024)
        .open(path)
        .unwrap();
    let transaction = db.begin_read().unwrap();
    for name in ["headers", "contexts"] {
        let table = transaction
            .open_table(TableDefinition::<u64, &[u8]>::new(name))
            .unwrap();
        assert_eq!(table.len().unwrap(), 52_560, "{name}");
    }
}

async fn send(stream: &mut TcpStream, message: NetworkMessage) -> std::io::Result<()> {
    stream
        .write_all(&serialize(&RawNetworkMessage::new(
            Network::Bitcoin.magic(),
            message,
        )))
        .await
}

async fn read(stream: &mut TcpStream) -> std::io::Result<NetworkMessage> {
    let mut header = [0; 24];
    stream.read_exact(&mut header).await?;
    let len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
    assert!(len <= 4096, "fixture control request exceeds bound");
    let mut bytes = header.to_vec();
    bytes.resize(24 + len, 0);
    stream.read_exact(&mut bytes[24..]).await?;
    Ok(deserialize::<RawNetworkMessage>(&bytes)
        .unwrap()
        .into_payload())
}

async fn peer(mut stream: TcpStream, fixture: &Fixture) -> std::io::Result<()> {
    let _live = observer::enter(Counter::Peer);
    assert!(matches!(
        read(&mut stream).await?,
        NetworkMessage::Version(_)
    ));
    let address = stream.local_addr()?;
    let services = ServiceFlags::NETWORK | ServiceFlags::WITNESS;
    let mut version = VersionMessage::new(
        services,
        0,
        Address::new(&address, services),
        Address::new(&address, services),
        42,
        "/offline-readiness/".into(),
        970127,
    );
    version.version = 70016;
    send(&mut stream, NetworkMessage::Version(version)).await?;
    send(&mut stream, NetworkMessage::Verack).await?;
    loop {
        match read(&mut stream).await? {
            NetworkMessage::Ping(nonce) => send(&mut stream, NetworkMessage::Pong(nonce)).await?,
            NetworkMessage::GetHeaders(_) => {
                send(&mut stream, NetworkMessage::Headers(vec![])).await?;
            }
            NetworkMessage::GetData(_) => {
                fixture.requests.fetch_add(1, Ordering::SeqCst);
                while fixture.hold.load(Ordering::SeqCst) {
                    fixture.release.notified().await;
                }
                let mut frame = Vec::with_capacity(24);
                frame.extend_from_slice(&Network::Bitcoin.magic().to_bytes());
                frame.extend_from_slice(b"block\0\0\0\0\0\0\0");
                frame.extend_from_slice(&(fixture.raw.len() as u32).to_le_bytes());
                frame.extend_from_slice(&sha256d::Hash::hash(&fixture.raw).to_byte_array()[..4]);
                stream.write_all(&frame).await?;
                stream.write_all(&fixture.raw).await?;
            }
            _ => {}
        }
    }
}

async fn until(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("natural occupancy was not reached within the finite phase");
}

async fn request(address: std::net::SocketAddr, body: &str) -> TcpStream {
    let socket = TcpSocket::new_v4().unwrap();
    // Real Linux receiver backpressure; no fake delayed writer completion.
    socket.set_recv_buffer_size(4096).unwrap();
    let mut stream = socket.connect(address).await.unwrap();
    stream
        .write_all(
            format!(
                "POST / HTTP/1.1\r\nAuthorization: Basic Z3VhcmRpYW46c2VjcmV0\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    stream
}

async fn drain(mut stream: TcpStream, expected: Arc<Vec<u8>>) {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        assert!(header.len() < 4096);
        header.push(stream.read_u8().await.unwrap());
    }
    assert!(header.starts_with(b"HTTP/1.1 200 OK\r\n"));
    let mut offset = 0;
    let mut buffer = [0; 16384];
    loop {
        let n = stream.read(&mut buffer).await.unwrap();
        if n == 0 {
            break;
        }
        assert_eq!(&buffer[..n], &expected[offset..offset + n]);
        offset += n;
    }
    assert_eq!(offset, expected.len());
}

fn print_sample(state: &State, label: &'static str) {
    let sample = state.snapshot(label);
    println!(
        "phase={} accepted={} writers={} blocked={} cheap={} network={} peers={} rss_kib={} hwm_kib={} fds={}",
        sample.label,
        sample.accepted,
        sample.writers,
        sample.blocked,
        sample.cheap,
        sample.network,
        sample.peers,
        sample.rss_kib,
        sample.hwm_kib,
        sample.fds
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "bounded offline release measurement requires pinned external headers and body"]
async fn default_store_real_pool_and_rpc_pressure() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("chain.redb");
    let (store, anchor) = replay(&path);
    drop(store);
    inspect_rows(&path);
    let store = ValidatedHeaderStore::open(&path, settings(), anchor).unwrap();
    let selected_tip = store.header(970127).unwrap().block_hash();
    let raw = bounded_file(
        std::env::var("READINESS_BLOCK").expect("READINESS_BLOCK"),
        4_000_000,
    );
    assert_eq!(
        sha256::Hash::hash(&raw).to_string(),
        "5018f2a401a852885a66531908e92d58c03fe86430473baab372fee23e7181c2"
    );
    let block: Block = deserialize(&raw).unwrap();
    assert_eq!(block.header, store.header(934996).unwrap());
    println!(
        "fixture_height=934996 bytes={} weight={} payload_slack={} weight_slack={} transactions={} witness_items={}",
        raw.len(),
        block.weight().to_wu(),
        4_000_000 - raw.len(),
        4_000_000 - block.weight().to_wu(),
        block.txdata.len(),
        block
            .txdata
            .iter()
            .flat_map(|tx| &tx.input)
            .map(|input| input.witness.len())
            .sum::<usize>()
    );
    let hash = block.block_hash();
    drop(block);
    let expected = Arc::new(
        serde_json::to_vec(&serde_json::json!({
            "id": 1, "result": raw.to_lower_hex_string(), "error": null
        }))
        .unwrap(),
    );
    let body_request =
        serde_json::json!({"id":1,"method":"getblock","params":[hash,0]}).to_string();
    let cheap_request = r#"{"id":1,"method":"getblockcount","params":[]}"#;
    let cheap_expected = Arc::new(br#"{"error":null,"id":1,"result":970127}"#.to_vec());
    let fixture = Arc::new(Fixture {
        raw,
        requests: AtomicUsize::new(0),
        hold: AtomicBool::new(false),
        release: Notify::new(),
    });
    let observations = Arc::new(State::default());
    observer::install(observations.clone());
    let mut peers = JoinSet::new();
    let mut addresses = Vec::new();
    for _ in 0..4 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addresses.push(listener.local_addr().unwrap());
        let fixture = fixture.clone();
        peers.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let _ = peer(stream, &fixture).await;
            }
        });
    }
    let mut sync = HeaderSync::new(
        store,
        SyncConfig {
            peers: addresses,
            peer_pool: PeerPoolConfig {
                rotation: Duration::from_secs(3600),
                ..PeerPoolConfig::manual_only()
            },
            handshake_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            retry_delay: Duration::from_millis(1),
            requests_per_visit: 1,
            candidate_headers: 144,
        },
    )
    .unwrap();
    for _ in 0..12 {
        assert!(sync.step(2_000_000_000).await.result.is_ok());
        if sync.peer_counts().0 == 4 {
            break;
        }
    }
    assert_eq!(sync.peer_counts().0, 4);
    let rpc = GuardianRpc::bind(
        GuardianRpcConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            auth: crate::guardian_rpc_config::RpcAuth::Basic {
                username: "guardian".into(),
                password: "secret".into(),
            },
            fake_bitcoind: false,
            allow_submission: false,
        },
        sync,
    )
    .await
    .unwrap();
    let address = rpc.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(rpc.run(
        async {
            let _ = stopped.await;
        },
        |_, _| {},
    ));
    let transport_started = Instant::now();
    print_sample(&observations, "warmed");

    // Stage each actual successful response into a Pending socket write before
    // requesting the next. Flooding the network queue would measure busy errors.
    let mut clients = Vec::new();
    for n in 1..=8 {
        clients.push(request(address, &body_request).await);
        until(|| observations.blocked.load(Ordering::SeqCst) == n).await;
    }
    assert_eq!(observations.accepted.load(Ordering::SeqCst), 8);
    assert_eq!(observations.peers.load(Ordering::SeqCst), 4);
    print_sample(&observations, "eight-real-blocked-writers");
    let mut drains = JoinSet::new();
    for client in clients {
        drains.spawn(drain(client, expected.clone()));
    }
    timeout(Duration::from_secs(10), async {
        while let Some(result) = drains.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
    until(|| observations.accepted.load(Ordering::SeqCst) == 0).await;

    // Maximum compatible occupancy: two blocked writers, one active body,
    // four queued cheap reads and one queued network request, in eight slots.
    let mut clients = Vec::new();
    for n in 1..=2 {
        clients.push(request(address, &body_request).await);
        until(|| observations.blocked.load(Ordering::SeqCst) == n).await;
    }
    let requests = fixture.requests.load(Ordering::SeqCst);
    fixture.hold.store(true, Ordering::SeqCst);
    clients.push(request(address, &body_request).await);
    until(|| fixture.requests.load(Ordering::SeqCst) > requests).await;
    let mut cheap = Vec::new();
    for _ in 0..4 {
        cheap.push(request(address, cheap_request).await);
    }
    clients.push(request(address, &body_request).await);
    until(|| {
        observations.cheap.load(Ordering::SeqCst) == 4
            && observations.network.load(Ordering::SeqCst) == 1
    })
    .await;
    print_sample(&observations, "full-queues-two-writers-active-body");
    fixture.hold.store(false, Ordering::SeqCst);
    fixture.release.notify_waiters();
    until(|| {
        observations.samples.lock().unwrap().iter().any(|sample| {
            sample.label == "decoded"
                && sample.accepted == 8
                && sample.blocked == 2
                && sample.cheap == 4
                && sample.network == 1
                && sample.peers == 4
        })
    })
    .await;
    {
        let samples = observations.samples.lock().unwrap();
        for sample in samples.iter().filter(|s| s.cheap == 4 && s.network == 1) {
            println!("natural_decode_sample={sample:?}");
        }
    }
    let mut drains = JoinSet::new();
    for client in clients {
        drains.spawn(drain(client, expected.clone()));
    }
    for client in cheap {
        drains.spawn(drain(client, cheap_expected.clone()));
    }
    timeout(Duration::from_secs(10), async {
        while let Some(result) = drains.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
    until(|| observations.accepted.load(Ordering::SeqCst) == 0).await;
    // With transport slots still available, surplus work must receive the real
    // queue-busy response rather than merely wait in the TCP listener backlog.
    fixture.hold.store(true, Ordering::SeqCst);
    let requests = fixture.requests.load(Ordering::SeqCst);
    let active = request(address, &body_request).await;
    until(|| fixture.requests.load(Ordering::SeqCst) > requests).await;
    let mut cheap = Vec::new();
    for _ in 0..4 {
        cheap.push(request(address, cheap_request).await);
    }
    let queued = request(address, &body_request).await;
    until(|| {
        observations.cheap.load(Ordering::SeqCst) == 4
            && observations.network.load(Ordering::SeqCst) == 1
    })
    .await;
    let busy = Arc::new(
        serde_json::to_vec(&serde_json::json!({
            "id": 1, "result": null,
            "error": {"code": -32014, "message": "busy; bounded operation queue is full",
                      "data": {"retryable": true}}
        }))
        .unwrap(),
    );
    drain(request(address, cheap_request).await, busy.clone()).await;
    drain(request(address, &body_request).await, busy).await;
    print_sample(&observations, "both-queues-reject-surplus");
    fixture.hold.store(false, Ordering::SeqCst);
    fixture.release.notify_waiters();
    let mut drains = JoinSet::new();
    drains.spawn(drain(active, expected.clone()));
    drains.spawn(drain(queued, expected));
    for client in cheap {
        drains.spawn(drain(client, cheap_expected.clone()));
    }
    timeout(Duration::from_secs(10), async {
        while let Some(result) = drains.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
    until(|| observations.accepted.load(Ordering::SeqCst) == 0).await;
    drain(
        request(address, cheap_request).await,
        cheap_expected.clone(),
    )
    .await;
    print_sample(&observations, "drained-recovered");

    // Cancel the same reachable eight-slot combination rather than only draining
    // successful responses. Closing clients must release slots, writers, queues,
    // and the active lease before the ordinary fourteen-second HTTP deadline.
    let mut canceled = Vec::new();
    for n in 1..=2 {
        canceled.push(request(address, &body_request).await);
        until(|| observations.blocked.load(Ordering::SeqCst) == n).await;
    }
    let requests = fixture.requests.load(Ordering::SeqCst);
    fixture.hold.store(true, Ordering::SeqCst);
    canceled.push(request(address, &body_request).await);
    until(|| fixture.requests.load(Ordering::SeqCst) > requests).await;
    for _ in 0..4 {
        canceled.push(request(address, cheap_request).await);
    }
    canceled.push(request(address, &body_request).await);
    until(|| {
        observations.accepted.load(Ordering::SeqCst) == 8
            && observations.cheap.load(Ordering::SeqCst) == 4
            && observations.network.load(Ordering::SeqCst) == 1
    })
    .await;
    print_sample(&observations, "cancel-eight-occupied");
    let cancellation_started = Instant::now();
    drop(canceled);
    until(|| {
        observations.accepted.load(Ordering::SeqCst) == 0
            && observations.blocked.load(Ordering::SeqCst) == 0
            && observations.cheap.load(Ordering::SeqCst) == 0
            && observations.network.load(Ordering::SeqCst) == 0
    })
    .await;
    println!(
        "cancellation_ms={}",
        cancellation_started.elapsed().as_millis()
    );
    // Let the fixture observe EOF on the canceled lease; it cannot observe that
    // EOF while intentionally waiting at its response gate.
    fixture.hold.store(false, Ordering::SeqCst);
    fixture.release.notify_waiters();
    until(|| observations.peers.load(Ordering::SeqCst) == 3).await;
    drain(request(address, cheap_request).await, cheap_expected).await;
    print_sample(&observations, "canceled-recovered");
    println!("transport_ms={}", transport_started.elapsed().as_millis());
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    peers.abort_all();
    while peers.join_next().await.is_some() {}
    assert_eq!(observations.peers.load(Ordering::SeqCst), 0);
    assert_eq!(observations.accepted.load(Ordering::SeqCst), 0);
    observer::clear();
    inspect_rows(&path);
    let store = ValidatedHeaderStore::open(&path, settings(), anchor).unwrap();
    assert_eq!(store.bounds().unwrap(), (917568, 970127));
    assert_eq!(store.header(970127).unwrap().block_hash(), selected_tip);
    assert_eq!(store.protected_height().unwrap(), anchor.height);
}
