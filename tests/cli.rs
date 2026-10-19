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
    command.env_remove("RUST_LOG").arg("--db").arg(db).args([
        "--capacity",
        "8",
        "--cache-mib",
        "1",
    ]);
    command
}

fn stdout(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn logging_defaults_overrides_and_stdout_separation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain.redb");
    create(&db);
    let baseline = command(&db).arg("status").output().unwrap();
    let debug = command(&db)
        .env("RUST_LOG", "fedimint_light_node=debug")
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(stdout(&baseline), stdout(&debug));
    assert!(!String::from_utf8_lossy(&baseline.stderr).contains("DEBUG"));
    assert!(String::from_utf8_lossy(&debug.stderr).contains("cli_start"));
    assert!(!debug.stderr.contains(&0x1b));

    std::fs::write(&db, b"invalid").unwrap();
    for (filter, info, errors) in [
        (None, true, true),
        (Some("error"), false, true),
        (Some("off"), false, false),
        (Some(""), true, true),
    ] {
        let mut cmd = command(&db);
        if let Some(filter) = filter {
            cmd.env("RUST_LOG", filter);
        }
        let result = cmd.output().unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert_eq!(stderr.contains("database_selected"), info, "{stderr}");
        assert_eq!(stderr.contains("cli_failed"), errors, "{stderr}");
    }
    let result = command(&db)
        .env("RUST_LOG", "secret-filter=not-a-level")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("invalid RUST_LOG; using info"));
    assert!(!stderr.contains("secret-filter"));
}

#[test]
fn logging_does_not_echo_source_credentials_or_affect_help() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain.redb");
    for source in [
        "https://secret-user:secret-password@example.invalid",
        "operator=https://secret-user:secret-password@example.invalid",
    ] {
        let result = command(&db)
            .env("RUST_LOG", "debug")
            .args(["bootstrap", "--source", source])
            .output()
            .unwrap();
        assert!(!result.status.success());
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!stderr.contains("secret-password"), "{stderr}");
        assert!(!stderr.contains("secret-user"), "{stderr}");
    }
    for flag in ["--help", "--version"] {
        let result = command(&db)
            .env("RUST_LOG", "invalid=secret")
            .arg(flag)
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(!result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }
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
fn stopped_binding_backup_restore_requires_current_independent_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    let headers = create(&db);
    let id = "0123456789abcdef".repeat(4);
    let hash = headers[2].block_hash().to_string();
    stdout(
        &command(&db)
            .args(["protect", "--height", "2", "--hash", &hash])
            .output()
            .unwrap(),
    );
    stdout(
        &command(&db)
            .args([
                "--guardian-id",
                &id,
                "bind-guardian",
                "--committed-height",
                "2",
                "--committed-hash",
                &hash,
                "--oldest-needed",
                "3",
            ])
            .output()
            .unwrap(),
    );
    let original = stdout(&command(&db).arg("status").output().unwrap());
    assert!(original.contains(&format!("guardian_id={id}")));
    {
        let store = ValidatedHeaderStore::open_pinned(
            &db,
            StoreConfig {
                header_capacity: 8.try_into().unwrap(),
                cache_bytes: (1024 * 1024).try_into().unwrap(),
            },
        )
        .unwrap();
        let bytes: [u8; 32] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
            .repeat(4)
            .try_into()
            .unwrap();
        assert_eq!(store.guardian_id().unwrap(), Some(bytes));
    }
    assert!(
        !command(&db)
            .args([
                "protect",
                "--height",
                "3",
                "--hash",
                &headers[3].block_hash().to_string()
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    let checkpoint = dir.path().join("current-state.json");
    stdout(
        &command(&db)
            .args(["--guardian-id", &id, "checkpoint", "--output"])
            .arg(&checkpoint)
            .output()
            .unwrap(),
    );
    let backup = dir.path().join("backup");
    let manifest = dir.path().join("manifest.json");
    stdout(
        &command(&db)
            .args(["--guardian-id", &id, "backup", "--output"])
            .arg(&backup)
            .arg("--manifest")
            .arg(&manifest)
            .output()
            .unwrap(),
    );
    let restored = dir.path().join("restored");
    let restore = |destination: &Path, identity: &str| {
        command(destination)
            .args(["--guardian-id", identity, "restore", "--backup"])
            .arg(&backup)
            .arg("--manifest")
            .arg(&manifest)
            .arg("--expected-state")
            .arg(&checkpoint)
            .output()
            .unwrap()
    };
    assert!(!restore(&restored, &"43".repeat(32)).status.success());
    assert!(!restored.exists());
    stdout(&restore(&restored, &id));
    assert_eq!(
        stdout(&command(&restored).arg("status").output().unwrap()),
        original
    );
    assert!(!restore(&restored, &id).status.success()); // No overwrite.
    // A newer independently retained floor rejects an otherwise intact old backup.
    let mut current: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&checkpoint).unwrap()).unwrap();
    current["protected_height"] = 3.into();
    current["protected_hash"] = headers[3].block_hash().to_string().into();
    std::fs::write(&checkpoint, serde_json::to_vec(&current).unwrap()).unwrap();
    let absent = dir.path().join("absent");
    assert!(!restore(&absent, &id).status.success());
    assert!(!absent.exists());
    // Restore is impossible with only the backup's own state and a missing manifest.
    std::fs::remove_file(&manifest).unwrap();
    assert!(!restore(&absent, &id).status.success());
    assert!(!absent.exists());
}

#[test]
fn backup_integrity_config_and_interrupted_publication_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    let backup = dir.path().join("backup");
    let manifest = dir.path().join("manifest");
    let checkpoint = dir.path().join("checkpoint");
    stdout(
        &command(&db)
            .args(["checkpoint", "--output"])
            .arg(&checkpoint)
            .output()
            .unwrap(),
    );
    stdout(
        &command(&db)
            .args(["backup", "--output"])
            .arg(&backup)
            .arg("--manifest")
            .arg(&manifest)
            .output()
            .unwrap(),
    );
    let destination = dir.path().join("restore");
    let restore = || {
        command(&destination)
            .args(["restore", "--backup"])
            .arg(&backup)
            .arg("--manifest")
            .arg(&manifest)
            .arg("--expected-state")
            .arg(&checkpoint)
            .output()
            .unwrap()
    };
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    // Even agreeing external records cannot relabel the actual snapshot state.
    let original = value.clone();
    for field in [
        "store_schema",
        "network",
        "height_one_hash",
        "anchor_height",
        "anchor_hash",
        "capacity",
        "retained_first",
        "tip_height",
        "tip_hash",
        "protected_height",
        "protected_hash",
        "guardian_id",
    ] {
        let mut changed = original.clone();
        changed["state"][field] = "wrong".into();
        std::fs::write(&manifest, serde_json::to_vec(&changed).unwrap()).unwrap();
        std::fs::write(&checkpoint, serde_json::to_vec(&changed["state"]).unwrap()).unwrap();
        assert!(!restore().status.success(), "{field}");
        assert!(!destination.exists(), "{field}");
    }
    std::fs::write(&checkpoint, serde_json::to_vec(&original["state"]).unwrap()).unwrap();
    value["sha256"] = "00".repeat(32).into();
    std::fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(!restore().status.success());
    assert!(!destination.exists());
    // A publication collision never replaces existing material.
    assert!(
        !command(&db)
            .args(["backup", "--output"])
            .arg(&backup)
            .arg("--manifest")
            .arg(dir.path().join("other"))
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!dir.path().join("other").exists());
    // Simulate a manifest-publication collision after successful DB publication:
    // different spellings of one nonexistent destination pass the early check.
    let interrupted = dir.path().join("interrupted");
    std::fs::create_dir(dir.path().join("child")).unwrap();
    let alias = dir.path().join("child").join("..").join("interrupted");
    let result = command(&db)
        .args(["backup", "--output"])
        .arg(&interrupted)
        .arg("--manifest")
        .arg(&alias)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(interrupted.exists()); // Complete data remains for inspection.
    assert!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(&alias).unwrap()).is_err());
}

#[cfg(unix)]
#[test]
fn bound_service_signal_and_kill_preserve_binding_and_exclude_online_backup() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    let headers = create(&db);
    let hash = headers[0].block_hash().to_string();
    let id = "31".repeat(32);
    stdout(
        &command(&db)
            .args([
                "--guardian-id",
                &id,
                "bind-guardian",
                "--committed-height",
                "0",
                "--committed-hash",
                &hash,
                "--oldest-needed",
                "1",
            ])
            .output()
            .unwrap(),
    );
    let credentials = dir.path().join("rpc.auth");
    std::fs::write(&credentials, "test:private\n").unwrap();
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600)).unwrap();
    for signal in ["TERM", "KILL"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let child = Child::start(
            command(&db)
                .args([
                    "--guardian-id",
                    &id,
                    "serve",
                    "--committed-height",
                    "0",
                    "--committed-hash",
                    &hash,
                    "--oldest-needed",
                    "1",
                    "--manual-only",
                    "--peer",
                    &listener.local_addr().unwrap().to_string(),
                    "--rpc-bind",
                    "127.0.0.1:0",
                    "--rpc-credentials",
                ])
                .arg(&credentials),
        );
        let mut stream = accept(&listener);
        read(&mut stream);
        let backup = dir.path().join("online-backup");
        let result = command(&db)
            .args(["--guardian-id", &id, "backup", "--output"])
            .arg(&backup)
            .arg("--manifest")
            .arg(dir.path().join("manifest"))
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!backup.exists());
        let pid = child.process.as_ref().unwrap().id().to_string();
        assert!(
            Command::new("sh")
                .args(["-c", "kill -\"$1\" \"$2\"", "sh", signal, &pid])
                .status()
                .unwrap()
                .success()
        );
        let result = child.finish();
        assert_eq!(result.status.success(), signal == "TERM");
        let status = stdout(&command(&db).arg("status").output().unwrap());
        assert!(status.contains(&format!("guardian_id={id}")));
        assert!(status.contains("protected_height=0"));
    }
    // Bound state cannot be accidentally exposed as an unbound smoke service.
    let result = command(&db)
        .args(["serve", "--standalone", "--rpc-credentials"])
        .arg(credentials)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let result = command(&db)
        .args(["--rpc-bind", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("use serve with explicit guardian preflight")
    );
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
        vec!["--fixed-fee-sats-per-kvb", "0", "status"],
        vec!["--fixed-fee-sats-per-kvb", "18446744073709551616", "status"],
        vec!["--fixed-fee-sats-per-kvb", "1.5", "status"],
        vec!["--fixed-fee-sats-per-kvb", "-1", "status"],
        vec!["sync", "--manual-only"],
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
fn rpc_configuration_conflicts_fail_before_creating_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("absent");
    for prefix in [vec![], vec!["serve", "--standalone"]] {
        for flags in [
            vec!["--rpc-no-auth", "--rpc-credentials", "/nonexistent"],
            vec!["--rpc-no-auth", "--allow-rpc-submission"],
            vec!["--allow-rpc-submission"],
            vec!["--rpc-no-auth", "--rpc-bind", "0.0.0.0:0"],
            vec!["--rpc-no-auth", "--rpc-bind", "[::]:0"],
        ] {
            let result = command(&db).args(&prefix).args(flags).output().unwrap();
            assert!(!result.status.success());
            assert!(!db.exists());
        }
    }
    let result = command(&db)
        .args([
            "--rpc-credentials",
            "/nonexistent",
            "serve",
            "--standalone",
            "--rpc-no-auth",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("after the explicit command"));
    let result = command(&db)
        .args(["serve", "--standalone"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&result.stderr).contains("--rpc-credentials or --rpc-no-auth"));
}

#[test]
fn normal_startup_never_replaces_invalid_existing_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("fedimint-light-node");
    std::fs::create_dir(&state).unwrap();
    let db = state.join("chain.redb");
    std::fs::write(&db, b"partial or invalid database").unwrap();
    // Literally no CLI arguments: the shipped binary resolves its persistent path.
    let result = Command::new(env!("CARGO_BIN_EXE_fedimint-light-node"))
        .env("XDG_STATE_HOME", dir.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    let diagnostics = String::from_utf8_lossy(&result.stderr);
    assert!(diagnostics.contains(db.to_str().unwrap()), "{diagnostics}");
    assert!(!diagnostics.contains("initializing new mainnet state"));
    assert_eq!(std::fs::read(&db).unwrap(), b"partial or invalid database");

    #[cfg(unix)]
    {
        std::fs::remove_file(&db).unwrap();
        let absent = state.join("absent-target");
        std::os::unix::fs::symlink(&absent, &db).unwrap();
        let result = command(&db)
            .args(["--rpc-bind", "127.0.0.1:0"])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            !String::from_utf8_lossy(&result.stderr).contains("initializing new mainnet state")
        );
        assert!(std::fs::symlink_metadata(&db).unwrap().is_symlink());
        assert!(!absent.exists());
    }
}

#[cfg(unix)]
#[test]
fn normal_startup_serves_existing_unbound_state_without_credentials_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    let peer = TcpListener::bind("127.0.0.1:0").unwrap();
    // Read the announced dynamic port instead of racing a released test socket.
    let mut child = Child::start(command(&db).args([
        "--manual-only",
        "--peer",
        &peer.local_addr().unwrap().to_string(),
        "--rpc-bind",
        "127.0.0.1:0",
    ]));
    let mut stream = accept(&peer);
    handshake(&mut stream);
    // Complete the first sync visit using only the local scripted peer.
    assert!(matches!(
        read(&mut stream),
        bitcoin::p2p::message::NetworkMessage::GetHeaders(_)
    ));
    send(
        &mut stream,
        bitcoin::p2p::message::NetworkMessage::Headers(vec![]),
    );
    let stderr = child.process.as_mut().unwrap().stderr.take().unwrap();
    let mut reader = std::io::BufReader::new(stderr);
    let mut diagnostics = String::new();
    let address = loop {
        use std::io::BufRead;
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        diagnostics.push_str(&line);
        if let Some((_, line)) = line.split_once("rpc_listen=") {
            assert!(line.contains("rpc_no_auth=true fake_bitcoind=false submission_only=false"));
            break line.split_whitespace().next().unwrap().to_string();
        }
    };
    let mut rpc = TcpStream::connect(&address).unwrap();
    rpc.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]}"#;
    write!(rpc, "POST / HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut response = String::new();
    rpc.read_to_string(&mut response).unwrap();
    assert!(response.contains("\"result\":3"), "{response}");
    let pid = child.process.as_ref().unwrap().id().to_string();
    assert!(
        Command::new("sh")
            .args(["-c", "kill -TERM \"$1\"", "sh", &pid])
            .status()
            .unwrap()
            .success()
    );
    assert!(child.finish().status.success());
    reader.read_to_string(&mut diagnostics).unwrap();
    assert!(
        diagnostics.contains("bitcoin-cli -datadir=/tmp -conf=/dev/null -rpcconnect=127.0.0.1")
    );
    assert!(!diagnostics.contains("-rpcuser"));
    assert!(!diagnostics.contains("-rpcpassword"));
    assert!(!diagnostics.contains("initializing new mainnet state"));
    assert!(stdout(&command(&db).arg("status").output().unwrap()).contains("selected_height=3"));
}

#[test]
fn fee_policy_is_explicit_positive_and_invocation_local() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    create(&db);
    for value in ["1", "2500", "18446744073709551615"] {
        let status = stdout(
            &command(&db)
                .args(["status", "--fixed-fee-sats-per-kvb", value])
                .output()
                .unwrap(),
        );
        assert!(status.contains("fee_policy=fixed-operator-supplied"));
        assert!(status.contains(&format!("fee_sats_per_kvb={value}\n")));
    }
    let status = stdout(&command(&db).arg("status").output().unwrap());
    assert!(status.contains("fee_policy=unavailable"));
    assert!(!status.contains("fee_sats_per_kvb="));
    assert!(status.contains("sync_progress=unavailable"));
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
        command(&db)
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
        .args(["bootstrap", "--manual-only", "--peer", "127.0.0.1:1"])
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
                "--manual-only",
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
    sync.args(["sync", "--once", "--manual-only"]);
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
            "--manual-only",
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
        "--manual-only",
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
                "--manual-only",
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
        "--manual-only",
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
        "--manual-only",
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
                "--manual-only",
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
            .args([
                "broadcast",
                "--manual-only",
                "--peer",
                "127.0.0.1:1",
                "--input",
            ])
            .arg(&input)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!db.exists());
    }
    let result = command(&db)
        .args([
            "broadcast",
            "--manual-only",
            "--peer",
            "127.0.0.1:1",
            "--input",
        ])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&result.stderr).contains("regular file"));
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        let result = command(&db)
            .args([
                "broadcast",
                "--manual-only",
                "--peer",
                "127.0.0.1:1",
                "--input",
            ])
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
                .args([
                    "broadcast",
                    "--manual-only",
                    "--peer",
                    "127.0.0.1:1",
                    "--input",
                ])
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
                "--manual-only",
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
