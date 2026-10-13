//! Real selected mainnet bodies through the pool, RPC owner and HTTP transport.
//! External fixtures are pinned by the same digests as the pressure campaign.

use std::sync::Mutex;

use super::*;

/// A shared finite fault schedule, not a replacement for production admission.
struct Faults {
    /// Checksum-valid body with a changed coinbase witness reserved value.
    corrupt: Vec<u8>,
    /// Which wire failure each scheduled hostile response produces.
    mode: &'static str,
    /// Remaining hostile responses before all peers serve the original body.
    remaining: AtomicUsize,
    /// Actual body requests in arrival order, including the serving peer.
    requests: Mutex<Vec<(usize, bool)>>,
    /// Hostile sockets whose client-side closure has been observed.
    closed: AtomicUsize,
    /// Maximum simultaneously accepted fixture-side P2P sockets.
    peak: AtomicUsize,
    /// Natural transport counters shared with the actual RPC owner.
    observations: Arc<State>,
}

async fn fault_peer(
    mut stream: TcpStream,
    id: usize,
    raw: &[u8],
    faults: &Faults,
) -> std::io::Result<()> {
    let _live = observer::enter(Counter::Peer);
    faults.peak.fetch_max(
        faults.observations.peers.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
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
        "/offline-body-recovery/".into(),
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
                let hostile = faults
                    .remaining
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok();
                faults.requests.lock().unwrap().push((id, hostile));
                let payload = if hostile { &faults.corrupt } else { raw };
                let mut frame = Vec::with_capacity(24);
                frame.extend_from_slice(&Network::Bitcoin.magic().to_bytes());
                frame.extend_from_slice(b"block\0\0\0\0\0\0\0");
                frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                frame.extend_from_slice(&sha256d::Hash::hash(payload).to_byte_array()[..4]);
                stream.write_all(&frame).await?;
                if hostile && faults.mode != "witness" {
                    stream.write_all(&payload[..7]).await?;
                    if faults.mode == "truncated" {
                        stream.shutdown().await?;
                    }
                } else {
                    stream.write_all(payload).await?;
                }
                if hostile {
                    assert_eq!(
                        timeout(Duration::from_secs(8), stream.read_u8())
                            .await
                            .expect("hostile lease was not closed")
                            .unwrap_err()
                            .kind(),
                        std::io::ErrorKind::UnexpectedEof
                    );
                    faults.closed.fetch_add(1, Ordering::SeqCst);
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

async fn response(address: std::net::SocketAddr, body: &str) -> serde_json::Value {
    timeout(Duration::from_secs(12), async {
        let mut stream = request(address, body).await;
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            assert!(header.len() < 4096);
            header.push(stream.read_u8().await.unwrap());
        }
        assert!(header.starts_with(b"HTTP/1.1 200 OK\r\n"));
        let mut bytes = Vec::new();
        // Even the test client bounds retained response bytes.
        stream
            .take(8_000_001)
            .read_to_end(&mut bytes)
            .await
            .unwrap();
        assert!(bytes.len() <= 8_000_000);
        serde_json::from_slice(&bytes).unwrap()
    })
    .await
    .expect("RPC response exceeded finite test deadline")
}

#[tokio::test]
#[ignore = "requires pinned READINESS_HEADERS and READINESS_BLOCK offline fixtures"]
async fn selected_body_failures_recover_through_managed_pool_and_rpc() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("chain.redb");
    let (mut store, anchor) = replay(&path);
    let raw = Arc::new(bounded_file(
        std::env::var("READINESS_BLOCK").expect("READINESS_BLOCK"),
        4_000_000,
    ));
    assert_eq!(
        sha256::Hash::hash(&raw).to_string(),
        "5018f2a401a852885a66531908e92d58c03fe86430473baab372fee23e7181c2"
    );
    let block: Block = deserialize(&raw).unwrap();
    assert_eq!(block.header, store.header(934996).unwrap());
    assert!(block.check_witness_commitment());
    let hash = block.block_hash();
    let tip = store.header(970127).unwrap().block_hash();
    let protection = (934995, store.header(934995).unwrap().block_hash());
    store.protect_through(protection.0, protection.1).unwrap();
    drop(store);
    let mut corrupt = block.clone();
    corrupt.txdata[0].input[0].witness = bitcoin::Witness::from_slice(&[[1; 32]]);
    assert_eq!(corrupt.header, block.header);
    assert!(corrupt.check_merkle_root());
    assert!(!corrupt.check_witness_commitment());
    let body_request =
        serde_json::json!({"id":1,"method":"getblock","params":[hash,0]}).to_string();

    for mode in ["witness", "truncated", "stalled"] {
        let observations = Arc::new(State::default());
        observer::install(observations.clone());
        let faults = Arc::new(Faults {
            corrupt: serialize(&corrupt),
            mode,
            remaining: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            closed: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            observations: observations.clone(),
        });
        let mut peers = JoinSet::new();
        let mut addresses = Vec::new();
        for id in 0..4 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            addresses.push(listener.local_addr().unwrap());
            let faults = faults.clone();
            let raw = raw.clone();
            peers.spawn(async move {
                // Accept concurrently so a leaked/duplicate client socket cannot
                // hide in a listener backlog behind its preceding connection.
                let mut connections = JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let (stream, _) = accepted.unwrap();
                            let faults = faults.clone();
                            let raw = raw.clone();
                            connections.spawn(async move {
                                let _ = fault_peer(stream, id, &raw, &faults).await;
                            });
                        }
                        result = connections.join_next(), if !connections.is_empty() => {
                            result.unwrap().unwrap();
                        }
                    }
                }
            });
        }
        let mut sync = HeaderSync::new(
            ValidatedHeaderStore::open(&path, settings(), anchor).unwrap(),
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
        for _ in 0..4 {
            assert!(sync.step(2_000_000_000).await.result.is_ok());
        }
        assert_eq!(sync.peer_counts().0, 4);
        let rpc = GuardianRpc::bind(
            GuardianRpcConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                username: "guardian".into(),
                password: "secret".into(),
                fake_bitcoind: false,
                allow_submission: false,
            },
            sync,
        )
        .await
        .unwrap();
        let address = rpc.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let owner = tokio::spawn(rpc.run(
            async {
                let _ = stopped.await;
            },
            move |sync, _| {
                let status = sync.status().unwrap();
                assert_eq!(
                    (status.height, status.hash, status.generation),
                    (970127, tip, 0)
                );
                assert!(!status.stopped);
                assert_eq!(sync.store().protected_checkpoint().unwrap(), protection);
                assert!(sync.peer_counts().0 <= 4);
            },
        ));

        // RPC promises two attempts, not unlimited retry. One hostile body can
        // recover in-request; two must report unavailable before a fresh request.
        for failures in [1, 2] {
            faults.requests.lock().unwrap().clear();
            faults.closed.store(0, Ordering::SeqCst);
            faults.remaining.store(failures, Ordering::SeqCst);
            let value = response(address, &body_request).await;
            if failures == 2 {
                assert!(value["result"].is_null(), "{mode}: {value}");
                assert_eq!(value["error"]["code"], -32012, "{mode}: {value}");
                assert_eq!(faults.requests.lock().unwrap().len(), 2);
            } else {
                assert!(value["error"].is_null());
                assert_eq!(value["result"].as_str().unwrap(), raw.to_lower_hex_string());
            }
            until(|| faults.closed.load(Ordering::SeqCst) == failures).await;
            let attempts = faults.requests.lock().unwrap().clone();
            assert_eq!(attempts.len(), 2);
            assert_ne!(attempts[0].0, attempts[1].0);
            assert!(attempts[0].1);
            assert_eq!(attempts[1].1, failures == 2);
            // Cheap work and another network request prove foreground ownership
            // was released; bytes must come from a different eligible peer.
            let cheap = response(address, r#"{"id":1,"method":"getblockcount","params":[]}"#).await;
            assert_eq!(cheap["result"], 970127);
            let value = response(address, &body_request).await;
            assert!(value["error"].is_null());
            assert_eq!(value["result"].as_str().unwrap(), raw.to_lower_hex_string());
            let attempts = faults.requests.lock().unwrap().clone();
            assert_eq!(attempts.len(), 3);
            assert!(!attempts[2].1);
            assert_ne!(attempts[0].0, attempts[2].0);
            if failures == 2 {
                assert_ne!(attempts[1].0, attempts[2].0);
            }
        }
        stop.send(()).unwrap();
        timeout(Duration::from_secs(3), owner)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        until(|| observations.peers.load(Ordering::SeqCst) == 0).await;
        assert_eq!(observations.accepted.load(Ordering::SeqCst), 0);
        assert_eq!(observations.writers.load(Ordering::SeqCst), 0);
        assert_eq!(observations.cheap.load(Ordering::SeqCst), 0);
        assert_eq!(observations.network.load(Ordering::SeqCst), 0);
        assert_eq!(faults.peak.load(Ordering::SeqCst), 4);
        peers.abort_all();
        while let Some(result) = peers.join_next().await {
            assert!(result.unwrap_err().is_cancelled());
        }
        observer::clear();
        let reopened = ValidatedHeaderStore::open(&path, settings(), anchor).unwrap();
        assert_eq!(reopened.bounds().unwrap(), (917568, 970127));
        assert_eq!(reopened.header(970127).unwrap().block_hash(), tip);
        assert_eq!(reopened.protected_checkpoint().unwrap(), protection);
        println!("{mode}: bounded retries, EOF cleanup, exact recovery and unchanged state");
    }
}
