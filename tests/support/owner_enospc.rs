//! Real storage failure in a disposable namespace, never a fabricated RPC reply.

use std::io::Write;

use fedimint_light_node::guardian_binding::GuardianPreflight;

use super::*;

/// Require the caller's exact private mount, before creating or filling anything.
fn private_mount() -> std::path::PathBuf {
    let root = std::fs::canonicalize(std::env::var("READINESS_FAULT_DIR").unwrap()).unwrap();
    let root_text = root.to_str().unwrap();
    assert!(!root_text.chars().any(char::is_whitespace));
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::path::PathBuf::from(
            std::env::var("READINESS_PARENT_MOUNT_NS")
                .expect("record mount namespace before unshare")
        ),
        "requires a private mount namespace"
    );
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    assert!(
        mounts.lines().any(|line| {
            let (mount, filesystem) = line.split_once(" - ").unwrap();
            let fields: Vec<_> = mount.split_whitespace().collect();
            let filesystem: Vec<_> = filesystem.split_whitespace().collect();
            fields[4] == root_text
                && filesystem[0] == "tmpfs"
                && filesystem[2]
                    .split(',')
                    .any(|option| option == "size=8192k")
        }),
        "requires an exact private 8 MiB tmpfs mount"
    );
    root
}

#[tokio::test]
#[ignore = "requires a private 8 MiB tmpfs in an unprivileged user/mount namespace"]
async fn actual_enospc_stops_owner_closes_clients_and_reopens_safely() {
    tokio::time::timeout(Duration::from_secs(180), exercise())
        .await
        .expect("bounded ENOSPC owner exercise");
}

async fn exercise() {
    let directory = tempfile::tempdir_in(private_mount()).unwrap();
    let path = directory.path().join("store.redb");
    let headers: Vec<Header> = include_bytes!("../data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .map(|bytes| deserialize(bytes).unwrap())
        .collect();
    let storage = StoreConfig {
        header_capacity: 512.try_into().unwrap(),
        cache_bytes: (1024 * 1024).try_into().unwrap(),
    };
    let anchor = StoreAnchor {
        network: Network::Bitcoin,
        height: 0,
        hash: headers[0].block_hash(),
        height_one_hash: headers[1].block_hash(),
    };
    let mut store = ValidatedHeaderStore::create(&path, storage, anchor, &headers[..1]).unwrap();
    store.select_suffix(0, &headers[1..=2], u64::MAX).unwrap();
    store.protect_through(1, headers[1].block_hash()).unwrap();
    let guardian = GuardianPreflight {
        guardian_id: [0x65; 32],
        committed_height: 1,
        committed_hash: headers[1].block_hash(),
        oldest_needed: 2,
    };
    store.bind_guardian(guardian).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sync = HeaderSync::new(
        store,
        SyncConfig {
            peers: vec![listener.local_addr().unwrap()],
            peer_pool: PeerPoolConfig::manual_only(),
            handshake_timeout: Duration::from_secs(2),
            request_timeout: Duration::from_secs(3),
            retry_delay: Duration::from_millis(1),
            requests_per_visit: 16,
            candidate_headers: 512,
        },
    )
    .unwrap()
    .with_fee_policy(FeePolicy::Fixed {
        sats_per_kvb: 1000.try_into().unwrap(),
    });
    let rpc = GuardianRpc::bind(rpc_config(false, false), sync)
        .await
        .unwrap();
    let address = rpc.local_addr().unwrap();
    let (ready, waiting) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let peer_headers = headers.clone();
    let peer_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(matches!(
            p2p_read(&mut stream).await.unwrap(),
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
            "/enospc/".into(),
            2035,
        );
        version.version = 70016;
        p2p_send(&mut stream, NetworkMessage::Version(version)).await;
        p2p_send(&mut stream, NetworkMessage::Verack).await;
        let mut gate = Some((ready, released));
        let mut sent = 2;
        while let Ok(message) = p2p_read(&mut stream).await {
            if let NetworkMessage::GetHeaders(request) = message {
                if let Some((ready, released)) = gate.take() {
                    ready.send(()).unwrap();
                    released.await.unwrap();
                }
                assert!(
                    request
                        .locator_hashes
                        .contains(&peer_headers[sent].block_hash())
                );
                let end = (sent + 512).min(peer_headers.len() - 1);
                assert!(
                    end > sent,
                    "all fixture headers committed without storage failure"
                );
                p2p_send(
                    &mut stream,
                    NetworkMessage::Headers(peer_headers[sent + 1..=end].to_vec()),
                )
                .await;
                sent = end;
            }
        }
        sent
    });
    let (failure, observed_failure) = std::sync::mpsc::channel();
    let owner = tokio::spawn(rpc.run(std::future::pending(), move |sync, diagnostic| {
        println!("{diagnostic}");
        if sync.status().unwrap().stopped {
            assert!(
                diagnostic.contains("Storage(") || diagnostic.contains("Commit("),
                "{diagnostic}"
            );
            assert!(
                diagnostic.contains("os error 28") || diagnostic.contains("code: 28"),
                "{diagnostic}"
            );
            assert!(diagnostic.contains("cheap_queued=1 network_queued=1 rpc_completed=0"));
            failure.send(()).unwrap();
        }
    }));
    waiting.await.unwrap();
    // The owner is now waiting for actual peer headers. Both complete queued reads
    // and a partial HTTP request are present before the failed durable update.
    let mut clients = Vec::new();
    for body in [
        json!({"id":1,"method":"getblockcount","params":[]}).to_string(),
        json!({"id":2,"method":"getblock","params":[headers[2].block_hash().to_string(),0]})
            .to_string(),
    ] {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(format!("POST / HTTP/1.1\r\nAuthorization: Basic Z3VhcmRpYW46c2VjcmV0\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        clients.push(stream);
    }
    let mut partial = TcpStream::connect(address).await.unwrap();
    partial.write_all(b"POST / HTTP/1.1\r\n").await.unwrap();
    clients.push(partial);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let filler = directory.path().join("fill");
    let mut file = std::fs::File::create(&filler).unwrap();
    let mut exhausted = false;
    // Defense in depth: even a mistaken mount cannot consume over 16 MiB.
    for _ in 0..256 {
        if let Err(error) = file.write_all(&[0; 65536]) {
            assert_eq!(error.raw_os_error(), Some(28));
            exhausted = true;
            break;
        }
    }
    assert!(exhausted, "bounded fill must establish real ENOSPC");
    drop(file);
    release.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(10), owner)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("explicit recovery"), "{error}");
    observed_failure
        .try_recv()
        .expect("observed terminal storage ENOSPC");
    let sent = tokio::time::timeout(Duration::from_secs(2), peer_task)
        .await
        .unwrap()
        .unwrap();
    for mut client in clients {
        let mut bytes = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut bytes))
            .await
            .unwrap();
        if let Err(error) = result {
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        }
        assert!(
            !String::from_utf8_lossy(&bytes).contains("\"result\""),
            "{bytes:?}"
        );
    }
    assert!(
        TcpStream::connect(address).await.is_err(),
        "failed owner still listening"
    );
    std::fs::remove_file(filler).unwrap();
    match ValidatedHeaderStore::open(&path, storage, anchor) {
        Ok(store) => {
            assert_eq!(store.anchor(), anchor);
            assert_eq!(store.guardian_id().unwrap(), Some(guardian.guardian_id));
            assert_eq!(
                store.protected_checkpoint().unwrap(),
                (1, headers[1].block_hash())
            );
            let (first, tip) = store.bounds().unwrap();
            assert!((2..=sent as u64).contains(&tip));
            assert!(tip == 2 || (tip - 2) % 512 == 0 || tip == 2035);
            assert_eq!(first, (tip + 1).saturating_sub(512));
            for height in first..=tip {
                assert_eq!(store.header(height).unwrap(), headers[height as usize]);
            }
            store
                .preflight_guardian(GuardianPreflight {
                    oldest_needed: first.max(2),
                    ..guardian
                })
                .unwrap();
            println!(
                "real owner ENOSPC: coherent reopen {first}..={tip}, binding and acknowledged floor retained"
            );
        }
        Err(error) => {
            assert!(path.exists());
            println!("real owner ENOSPC: explicit reopen refusal, no reset: {error}");
        }
    }
}
