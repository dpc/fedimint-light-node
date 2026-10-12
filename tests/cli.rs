use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use bitcoin::Network;
use bitcoin::block::Header;
use fedimint_light_node::store_anchor::StoreAnchor;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;

fn command(db: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fedimint-light-node"));
    command
        .arg("--db")
        .arg(db)
        .args(["--capacity", "8", "--cache-mib", "1"]);
    command
}

fn stdout(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn create(db: &Path) -> Vec<Header> {
    let headers: Vec<Header> = include_bytes!("data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .take(4)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect();
    let config = StoreConfig {
        header_capacity: 8.try_into().unwrap(),
        cache_bytes: (1024 * 1024).try_into().unwrap(),
    };
    let anchor = StoreAnchor {
        network: Network::Bitcoin,
        height_one_hash: headers[1].block_hash(),
        height: 0,
        hash: headers[0].block_hash(),
    };
    let mut store = ValidatedHeaderStore::create(db, config, anchor, &headers[..1]).unwrap();
    store.select_suffix(0, &headers[1..], u64::MAX).unwrap();
    headers
}

#[test]
fn help_and_argument_errors_are_offline() {
    let help = Command::new(env!("CARGO_BIN_EXE_fedimint-light-node"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(stdout(&help).contains("bootstrap"));
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("absent");
    for args in [
        vec!["--capacity", "0", "status"],
        vec!["--capacity", "105121", "status"],
        vec!["--cache-mib", "65", "status"],
        vec!["sync"],
        vec!["sync", "--peer", "hostname:8333"],
        vec!["sync", "--peer", "127.0.0.1:1", "--requests", "17"],
        vec!["fetch", "--peer", "127.0.0.1:1", "--height", "0"],
        vec!["protect", "--height", "0", "--hash", "garbage"],
        vec![
            "bootstrap",
            "--peer",
            "127.0.0.1:1",
            "--source",
            "a=http://localhost",
        ],
        vec![
            "bootstrap",
            "--peer",
            "127.0.0.1:1",
            "--source",
            "a=https://user:pass@example.com",
        ],
    ] {
        assert!(
            !Command::new(env!("CARGO_BIN_EXE_fedimint-light-node"))
                .arg("--db")
                .arg(&db)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(!db.exists());
    }
}

#[test]
fn open_never_creates_and_capacity_must_match() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("absent");
    assert!(
        !command(&db)
            .arg("status")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!db.exists());
    create(&db);
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_fedimint-light-node"))
            .arg("--db")
            .arg(&db)
            .args(["--capacity", "9", "status"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn status_and_protection_preserve_pinned_identity_and_honest_count() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    let headers = create(&db);
    let status = stdout(&command(&db).arg("status").output().unwrap());
    for expected in [
        "selected_height=3",
        "selected_count=4",
        "retained_first=0",
        "retained_last=3",
        "retained_headers=4",
        "checkpoint_height=0",
        "protected_height=0",
        "freshness=unproven",
        "body_availability=unproven",
        "validation=mechanical-not-full-consensus",
    ] {
        assert!(status.lines().any(|line| line == expected), "{status}");
    }
    assert!(status.contains(&format!("height_one_hash={}", headers[1].block_hash())));
    assert!(status.contains(&format!("checkpoint_hash={}", headers[0].block_hash())));
    let wrong = command(&db)
        .args([
            "protect",
            "--height",
            "2",
            "--hash",
            &headers[1].block_hash().to_string(),
        ])
        .output()
        .unwrap();
    assert!(!wrong.status.success());
    let protected = command(&db)
        .args([
            "protect",
            "--height",
            "2",
            "--hash",
            &headers[2].block_hash().to_string(),
        ])
        .output()
        .unwrap();
    assert!(stdout(&protected).contains("protected_height=2"));
    let reopened = stdout(&command(&db).arg("status").output().unwrap());
    assert!(reopened.contains("protected_height=2"));
    assert!(
        !command(&db)
            .args([
                "protect",
                "--height",
                "1",
                "--hash",
                &headers[1].block_hash().to_string()
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn bootstrap_refuses_existing_destination_without_network_or_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("existing");
    std::fs::write(&db, b"do not replace").unwrap();
    let output = command(&db)
        .args(["bootstrap", "--peer", "127.0.0.1:1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
    assert_eq!(std::fs::read(&db).unwrap(), b"do not replace");
}

#[test]
fn fetch_refuses_database_and_aliases_without_changing_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    let alias = dir.path().join("alias");
    std::fs::hard_link(&db, &alias).unwrap();
    let before = std::fs::read(&db).unwrap();
    let mut paths = vec![db.clone(), alias];
    #[cfg(unix)]
    {
        let symlink = dir.path().join("symlink");
        std::os::unix::fs::symlink(&db, &symlink).unwrap();
        paths.push(symlink);
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("missing"), &dangling).unwrap();
        paths.push(dangling);
    }
    for path in paths {
        let result = command(&db)
            .args([
                "fetch",
                "--peer",
                "127.0.0.1:1",
                "--height",
                "0",
                "--output",
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("output already exists"));
    }
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

#[test]
fn invalid_peer_count_and_future_fetch_fail_without_creation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    let mut sync = command(&db);
    sync.args(["sync", "--once"]);
    for _ in 0..17 {
        sync.args(["--peer", "127.0.0.1:1"]);
    }
    assert!(
        String::from_utf8_lossy(&sync.output().unwrap().stderr)
            .contains("invalid synchronization configuration")
    );
    let target = dir.path().join("block");
    let result = command(&db)
        .args([
            "fetch",
            "--peer",
            "127.0.0.1:1",
            "--height",
            "999",
            "--output",
        ])
        .arg(&target)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!target.exists());
}

/// Always reap the child, including when a scripted peer assertion fails.
struct Child {
    /// Running CLI with bounded captured output.
    process: Option<std::process::Child>,
}

impl Child {
    fn start(command: &mut Command) -> Self {
        Self {
            process: Some(
                command
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            ),
        }
    }

    fn finish(mut self) -> Output {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.process.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "CLI failed to exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        self.process.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "CLI did not connect");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

fn read(stream: &mut TcpStream) -> bitcoin::p2p::message::NetworkMessage {
    use bitcoin::p2p::message::RawNetworkMessage;
    let mut header = [0; 24];
    stream.read_exact(&mut header).unwrap();
    let len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
    assert!(len < 2_000_000);
    let mut bytes = header.to_vec();
    bytes.resize(24 + len, 0);
    stream.read_exact(&mut bytes[24..]).unwrap();
    bitcoin::consensus::deserialize::<RawNetworkMessage>(&bytes)
        .unwrap()
        .into_payload()
}

fn send(stream: &mut TcpStream, message: bitcoin::p2p::message::NetworkMessage) {
    stream
        .write_all(&bitcoin::consensus::serialize(
            &bitcoin::p2p::message::RawNetworkMessage::new(Network::Bitcoin.magic(), message),
        ))
        .unwrap();
}

fn handshake(stream: &mut TcpStream) {
    use bitcoin::p2p::ServiceFlags;
    use bitcoin::p2p::address::Address;
    use bitcoin::p2p::message::NetworkMessage;
    use bitcoin::p2p::message_network::VersionMessage;
    assert!(matches!(read(stream), NetworkMessage::Version(_)));
    let services = ServiceFlags::NETWORK | ServiceFlags::WITNESS;
    let address = Address::new(&stream.local_addr().unwrap(), services);
    let mut version = VersionMessage::new(
        services,
        0,
        address.clone(),
        address,
        42,
        "/cli-test/".into(),
        3,
    );
    version.version = 70016;
    version.relay = true;
    send(stream, NetworkMessage::Version(version));
    send(stream, NetworkMessage::Verack);
    assert_eq!(read(stream), NetworkMessage::Verack);
}

#[test]
fn loopback_sync_once_and_fetch_publish_real_block_bytes() {
    use bitcoin::p2p::message::NetworkMessage;
    use bitcoin::p2p::message_blockdata::Inventory;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = listener.local_addr().unwrap().to_string();
    let child = Child::start(command(&db).args([
        "sync",
        "--peer",
        &peer,
        "--once",
        "--retry-seconds",
        "1",
    ]));
    let mut stream = accept(&listener);
    handshake(&mut stream);
    assert!(matches!(read(&mut stream), NetworkMessage::GetHeaders(_)));
    send(&mut stream, NetworkMessage::Headers(vec![]));
    let result = child.finish();
    assert!(stdout(&result).contains("freshness=unproven"));
    drop(stream);

    let path = dir.path().join("genesis.raw");
    let child = Child::start(
        command(&db)
            .args([
                "fetch",
                "--peer",
                &peer,
                "--height",
                "0",
                "--retry-seconds",
                "1",
                "--output",
            ])
            .arg(&path),
    );
    let mut stream = accept(&listener);
    handshake(&mut stream);
    let genesis = bitcoin::blockdata::constants::genesis_block(Network::Bitcoin);
    assert_eq!(
        read(&mut stream),
        NetworkMessage::GetData(vec![Inventory::WitnessBlock(genesis.block_hash())])
    );
    send(&mut stream, NetworkMessage::Block(genesis.clone()));
    assert!(child.finish().status.success());
    assert_eq!(
        std::fs::read(path).unwrap(),
        bitcoin::consensus::serialize(&genesis)
    );
    assert!(stdout(&command(&db).arg("status").output().unwrap()).contains("protected_height=0"));
}

#[test]
fn stalled_peer_once_exits_with_error_without_claiming_health() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = listener.local_addr().unwrap().to_string();
    let child = Child::start(command(&db).args([
        "sync",
        "--peer",
        &peer,
        "--once",
        "--retry-seconds",
        "1",
        "--timeout-seconds",
        "1",
    ]));
    let mut stream = accept(&listener);
    read(&mut stream); // Send nothing: whole handshake deadline must expire.
    let result = child.finish();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("peer deadline expired"));
    assert!(String::from_utf8_lossy(&result.stdout).contains("freshness=unproven"));
}

#[cfg(unix)]
#[test]
fn sigterm_cancels_owned_socket_and_allows_offline_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = listener.local_addr().unwrap().to_string();
    let child = Child::start(command(&db).args([
        "sync",
        "--peer",
        &peer,
        "--retry-seconds",
        "1",
        "--timeout-seconds",
        "60",
    ]));
    let mut stream = accept(&listener);
    read(&mut stream); // Connection proves the signal handlers were installed.
    let pid = child.process.as_ref().unwrap().id().to_string();
    assert!(
        Command::new("sh")
            .args(["-c", "kill -TERM \"$1\"", "sh", &pid])
            .status()
            .unwrap()
            .success()
    );
    let result = child.finish();
    assert_eq!(result.status.code(), Some(130));
    assert!(String::from_utf8_lossy(&result.stdout).contains("selected_height=3"));
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    assert!(
        command(&db)
            .arg("status")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn broadcast_delivers_witness_bytes_and_reports_only_local_write_success() {
    use bitcoin::p2p::message::NetworkMessage;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    let input = dir.path().join("transaction.raw");
    let mut tx = bitcoin::blockdata::constants::genesis_block(Network::Bitcoin)
        .txdata
        .remove(0);
    tx.input[0].witness = bitcoin::Witness::from_slice(&[b"witness bytes"]);
    let bytes = bitcoin::consensus::serialize(&tx);
    std::fs::write(&input, &bytes).unwrap();
    let before = stdout(&command(&db).arg("status").output().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let child = Child::start(
        command(&db)
            .args([
                "broadcast",
                "--peer",
                &listener.local_addr().unwrap().to_string(),
                "--retry-seconds",
                "1",
                "--input",
            ])
            .arg(&input),
    );
    let mut stream = accept(&listener);
    handshake(&mut stream);
    assert_eq!(read(&mut stream), NetworkMessage::Tx(tx));
    let NetworkMessage::Ping(nonce) = read(&mut stream) else {
        panic!("expected ping")
    };
    send(&mut stream, NetworkMessage::Pong(nonce));
    let output = stdout(&child.finish());
    assert!(output.contains("submitted=1"));
    assert!(output.contains("submitted-local-write-only"));
    assert!(output.contains("pong-not-acceptance"));
    assert!(output.contains("acceptance=unproven propagation=unproven confirmation=unproven"));
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
    assert_eq!(
        stdout(&command(&db).arg("status").output().unwrap()),
        before
    );
}

#[test]
fn broadcast_rejects_malformed_oversized_and_nonregular_files_offline() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("absent");
    let input = dir.path().join("tx");
    for bytes in [vec![], b"0100000001".to_vec(), vec![0; 400_001]] {
        std::fs::write(&input, bytes).unwrap();
        let result = command(&db)
            .args(["broadcast", "--peer", "127.0.0.1:1", "--input"])
            .arg(&input)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!db.exists());
    }
    let result = command(&db)
        .args(["broadcast", "--peer", "127.0.0.1:1", "--input"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&result.stderr).contains("regular file"));
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        let result = command(&db)
            .args(["broadcast", "--peer", "127.0.0.1:1", "--input"])
            .arg(&link)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&result.stderr).contains("regular file"));
        let fifo = dir.path().join("fifo");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let child = Child::start(
            command(&db)
                .args(["broadcast", "--peer", "127.0.0.1:1", "--input"])
                .arg(&fifo),
        );
        assert!(String::from_utf8_lossy(&child.finish().stderr).contains("regular file"));
    }
}

#[cfg(unix)]
#[test]
fn broadcast_sigterm_after_delivery_reports_indeterminate_and_releases_store() {
    use bitcoin::p2p::message::NetworkMessage;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    let input = dir.path().join("tx");
    let tx = bitcoin::blockdata::constants::genesis_block(Network::Bitcoin)
        .txdata
        .remove(0);
    std::fs::write(&input, bitcoin::consensus::serialize(&tx)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let child = Child::start(
        command(&db)
            .args([
                "broadcast",
                "--peer",
                &listener.local_addr().unwrap().to_string(),
                "--retry-seconds",
                "1",
                "--input",
            ])
            .arg(&input),
    );
    let mut stream = accept(&listener);
    handshake(&mut stream);
    assert!(matches!(read(&mut stream), NetworkMessage::Tx(_)));
    assert!(matches!(read(&mut stream), NetworkMessage::Ping(_)));
    let pid = child.process.as_ref().unwrap().id().to_string();
    assert!(
        Command::new("sh")
            .args(["-c", "kill -TERM \"$1\"", "sh", &pid])
            .status()
            .unwrap()
            .success()
    );
    let result = child.finish();
    assert_eq!(result.status.code(), Some(130));
    assert!(String::from_utf8_lossy(&result.stderr).contains("delivery is indeterminate"));
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    assert!(
        command(&db)
            .arg("status")
            .output()
            .unwrap()
            .status
            .success()
    );
}
