//! Disposable process-crash schedules; these do not simulate power loss or ENOSPC.
#![cfg(unix)]

use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use bitcoin::Network;
use bitcoin::block::Header;
use fedimint_light_node::guardian_binding::GuardianPreflight;
use fedimint_light_node::store_anchor::StoreAnchor;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;

fn headers() -> Vec<Header> {
    include_bytes!("data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect()
}

fn config() -> StoreConfig {
    StoreConfig {
        header_capacity: 512.try_into().unwrap(),
        cache_bytes: (8 * 1024 * 1024).try_into().unwrap(),
    }
}

fn anchor(headers: &[Header]) -> StoreAnchor {
    StoreAnchor {
        network: Network::Bitcoin,
        height: 0,
        hash: headers[0].block_hash(),
        height_one_hash: headers[1].block_hash(),
    }
}

#[test]
fn crash_writer_child() {
    let Ok(path) = std::env::var("READINESS_CRASH_CHILD") else {
        return;
    };
    let headers = headers();
    let mut store = ValidatedHeaderStore::open(path, config(), anchor(&headers)).unwrap();
    for height in 1..headers.len() {
        store
            .select_suffix(height as u64 - 1, &headers[height..=height], u64::MAX)
            .unwrap();
        println!("ACK {height}");
        if std::env::var_os("READINESS_ALLOCATION_PRESSURE").is_some() {
            std::hint::black_box(vec![0u8; 128 * 1024 * 1024]);
        }
    }
}

const GUARDIAN: [u8; 32] = [0x73; 32];
const PRUNED_FLOOR: u64 = 600;
const NEXT_FLOOR: u64 = 900;
const PRUNED_TIP: u64 = 1200;

fn guardian_state(headers: &[Header], height: u64, oldest_needed: u64) -> GuardianPreflight {
    GuardianPreflight {
        guardian_id: GUARDIAN,
        committed_height: height,
        committed_hash: headers[height as usize].block_hash(),
        oldest_needed,
    }
}

fn child_event(event: &str) {
    println!("{event}");
    std::io::stdout().flush().unwrap();
}

#[test]
fn protection_crash_child() {
    let Ok(path) = std::env::var("READINESS_PROTECTION_CHILD") else {
        return;
    };
    let headers = headers();
    let mut store = ValidatedHeaderStore::open(path, config(), anchor(&headers)).unwrap();
    store
        .protect_through(PRUNED_FLOOR, headers[PRUNED_FLOOR as usize].block_hash())
        .unwrap();
    // ACK follows the public API's durable successful return, never its invocation.
    child_event("ACK 600");
    store
        .select_suffix(700, &headers[701..=PRUNED_TIP as usize], u64::MAX)
        .unwrap();
    assert!(store.bounds().unwrap().0 > PRUNED_FLOOR);
    child_event("PRUNED");
    child_event("BEFORE 900");
    let mut command = String::new();
    std::io::stdin().read_line(&mut command).unwrap();
    assert_eq!(command, "GO\n");
    store
        .protect_through(NEXT_FLOOR, headers[NEXT_FLOOR as usize].block_hash())
        .unwrap();
    child_event("ACK 900");
    // Keep the live store open until SIGKILL, including after the final ACK.
    loop {
        std::thread::park();
    }
}

/// Kill and reap even if an assertion or a bounded event wait fails.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn bound_protection_crash_reopen_preserves_advanced_pruned_checkpoint() {
    let headers = headers();
    let anchor = anchor(&headers);
    // Include deterministic before-call and after-ACK controls, then race the
    // public operation. No production hooks or claims of exact internal cutpoints.
    for schedule in 0..18 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("protection.redb");
        let mut store =
            ValidatedHeaderStore::create(&path, config(), anchor, &headers[..1]).unwrap();
        store.bind_guardian(guardian_state(&headers, 0, 0)).unwrap();
        store.select_suffix(0, &headers[1..=512], u64::MAX).unwrap();
        store
            .select_suffix(512, &headers[513..=700], u64::MAX)
            .unwrap();
        drop(store);
        let mut child = KillOnDrop(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "protection_crash_child", "--nocapture"])
                .env("READINESS_PROTECTION_CHILD", &path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let stdout = child.0.stdout.take().unwrap();
        let (send, events) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                send.send(line.unwrap()).unwrap();
            }
        });
        let mut acknowledgements = Vec::new();
        let mut wait_for = |expected: &str| loop {
            let line = events.recv_timeout(Duration::from_secs(30)).unwrap();
            if let Some(height) = line.strip_prefix("ACK ") {
                acknowledgements.push(height.parse::<u64>().unwrap());
            }
            if line == expected {
                break;
            }
        };
        wait_for("ACK 600");
        wait_for("PRUNED");
        wait_for("BEFORE 900");
        if schedule != 0 {
            child.0.stdin.as_mut().unwrap().write_all(b"GO\n").unwrap();
            if schedule == 1 {
                wait_for("ACK 900");
            } else {
                std::thread::sleep(Duration::from_micros(
                    [0, 50, 100, 250, 500, 1_000, 2_000, 5_000][(schedule - 2) % 8],
                ));
            }
        }
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        reader.join().unwrap();
        for line in events.try_iter() {
            if let Some(height) = line.strip_prefix("ACK ") {
                acknowledgements.push(height.parse::<u64>().unwrap());
            }
        }
        let store = ValidatedHeaderStore::open(&path, config(), anchor).unwrap();
        assert_eq!(store.anchor(), anchor);
        assert_eq!(store.guardian_id().unwrap(), Some(GUARDIAN));
        let (first, tip) = store.bounds().unwrap();
        assert_eq!((first, tip), (689, PRUNED_TIP));
        assert!(store.header(PRUNED_FLOOR).is_err());
        let (floor, hash) = store.protected_checkpoint().unwrap();
        assert!([PRUNED_FLOOR, NEXT_FLOOR].contains(&floor));
        assert_eq!(store.protected_height().unwrap(), floor);
        assert_eq!(hash, headers[floor as usize].block_hash());
        assert!(acknowledgements.contains(&PRUNED_FLOOR));
        for height in &acknowledgements {
            assert!(*height <= floor, "lost acknowledged protection {height}");
            // Earlier pruned checkpoints need not remain queryable once advanced;
            // the exact successor's fixture ancestry must still include their hash.
            assert!([PRUNED_FLOOR, NEXT_FLOOR].contains(height));
        }
        if schedule == 0 {
            assert_eq!(floor, PRUNED_FLOOR);
        }
        if schedule == 1 {
            assert_eq!(floor, NEXT_FLOOR);
            assert!(acknowledgements.contains(&NEXT_FLOOR));
        }
        for height in first..=tip {
            assert_eq!(store.header(height).unwrap(), headers[height as usize]);
        }
        assert_eq!(store.tip_context().unwrap().height(), tip);
        let expected = guardian_state(&headers, floor, first);
        store.preflight_guardian(expected).unwrap();
        assert!(
            store
                .preflight_guardian(GuardianPreflight {
                    guardian_id: [0x74; 32],
                    ..expected
                })
                .is_err()
        );
        assert!(
            store
                .preflight_guardian(GuardianPreflight {
                    committed_hash: anchor.hash,
                    ..expected
                })
                .is_err()
        );
        println!(
            "schedule={schedule} acknowledged={acknowledgements:?} checkpoint={floor}:{hash} retained={first}..={tip}"
        );
    }
}

#[test]
#[ignore = "explicit bounded process-crash campaign"]
fn seeded_kill_reopen_keeps_committed_chain() {
    let headers = headers();
    let anchor = anchor(&headers);
    let mut seed = 0x5eed_cafe_u64;
    let mut max_tip = 0;
    for schedule in 0..32 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("crash.redb");
        drop(ValidatedHeaderStore::create(&path, config(), anchor, &headers[..1]).unwrap());
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_writer_child", "--nocapture"])
            .env("READINESS_CRASH_CHILD", &path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let delay = if schedule < 28 {
            1 + seed % 100
        } else {
            1000 + seed % 2000
        };
        std::thread::sleep(Duration::from_millis(delay));
        child.kill().unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.signal(), Some(9));
        let acknowledged = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| {
                line.split_once("ACK ")
                    .and_then(|(_, height)| height.parse::<u64>().ok())
            })
            .max()
            .unwrap_or(0);
        // Reopen must succeed from a completely created old store, with every
        // retained row matching the known chain. A refused/partial commit fails.
        let store = ValidatedHeaderStore::open(&path, config(), anchor).unwrap();
        assert_eq!(store.anchor(), anchor);
        assert_eq!(store.protected_height().unwrap(), anchor.height);
        let (first, tip) = store.bounds().unwrap();
        assert!(tip < headers.len() as u64);
        assert!(
            (acknowledged..=acknowledged + 1).contains(&tip),
            "tip {tip} is neither last acknowledged commit {acknowledged} nor its in-flight successor"
        );
        assert_eq!(store.tip_context().unwrap().height(), tip);
        for height in first..=tip {
            assert_eq!(store.header(height).unwrap(), headers[height as usize]);
        }
        max_tip = max_tip.max(tip);
        println!(
            "schedule={schedule} delay_ms={delay} acknowledged={acknowledged} retained={first}..={tip}"
        );
    }
    assert!(max_tip > 0, "campaign never reached a durable descendant");
}

#[test]
fn read_only_store_refuses_writer_without_reset() {
    let headers = headers();
    let anchor = anchor(&headers);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("readonly.redb");
    let store = ValidatedHeaderStore::create(&path, config(), anchor, &headers[..1]).unwrap();
    // Exclusive ownership also prevents a second writer before any mutation.
    assert!(ValidatedHeaderStore::open(&path, config(), anchor).is_err());
    drop(store);
    // This permission test requires an unprivileged process, as the service does.
    // Root/CAP_DAC_OVERRIDE bypasses mode bits and is not a valid test environment.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    assert!(ValidatedHeaderStore::open(&path, config(), anchor).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let store = ValidatedHeaderStore::open(&path, config(), anchor).unwrap();
    assert_eq!(store.anchor(), anchor);
    assert_eq!(store.protected_height().unwrap(), 0);
    assert_eq!(store.bounds().unwrap(), (0, 0));
}

#[test]
#[ignore = "explicit bounded RLIMIT_AS allocation-abort experiment, not kernel OOM"]
fn address_limit_abort_preserves_acknowledged_commit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("allocation.redb");
    let headers = headers();
    let anchor = anchor(&headers);
    drop(ValidatedHeaderStore::create(&path, config(), anchor, &headers[..1]).unwrap());
    let output = Command::new("sh")
        .args([
            "-c",
            "set -eu; ulimit -c 0; ulimit -v 65536; exec \"$1\" --exact crash_writer_child --nocapture",
            "sh",
        ])
        .arg(std::env::current_exe().unwrap())
        .env("READINESS_CRASH_CHILD", &path)
        .env("READINESS_ALLOCATION_PRESSURE", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.signal(), Some(6), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("ACK 1"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("memory allocation"));
    let store = ValidatedHeaderStore::open(&path, config(), anchor).unwrap();
    assert_eq!(store.anchor(), anchor);
    assert_eq!(store.protected_height().unwrap(), 0);
    assert_eq!(store.bounds().unwrap(), (0, 1));
    assert_eq!(store.header(1).unwrap(), headers[1]);
}

#[test]
#[ignore = "requires an explicitly bounded private tmpfs in a user mount namespace"]
fn constrained_filesystem_failure() {
    let root = std::env::var("READINESS_FAULT_DIR").expect("private bounded tmpfs");
    let mode = std::env::var("READINESS_FAULT_MODE").expect("bytes or inodes");
    let directory = tempfile::tempdir_in(root).unwrap();
    let path = directory.path().join("store.redb");
    let headers = headers();
    let anchor = anchor(&headers);
    let mut store = ValidatedHeaderStore::create(&path, config(), anchor, &headers[..1]).unwrap();
    let mut fillers = Vec::new();
    let mut exhausted = false;
    if mode == "bytes" {
        let filler = directory.path().join("fill");
        let mut file = std::fs::File::create(&filler).unwrap();
        fillers.push(filler);
        // Refuse to consume more than16MiB even if the caller forgot the mount.
        for _ in 0..256 {
            match file.write_all(&[0; 65536]) {
                Ok(()) => {}
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(28), "expected ENOSPC");
                    exhausted = true;
                    break;
                }
            }
        }
    } else {
        assert_eq!(mode, "inodes");
        for index in 0..256 {
            let filler = directory.path().join(format!("fill-{index}"));
            match std::fs::File::create(&filler) {
                Ok(_) => fillers.push(filler),
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(28), "expected ENOSPC");
                    exhausted = true;
                    break;
                }
            }
        }
    }
    assert!(exhausted, "private tmpfs did not hit expected finite limit");
    if mode == "bytes" {
        let mut failed = false;
        for height in 1..headers.len() {
            if store
                .select_suffix(height as u64 - 1, &headers[height..=height], u64::MAX)
                .is_err()
            {
                failed = true;
                break;
            }
        }
        assert!(failed, "full filesystem did not exercise an update failure");
    } else {
        assert!(
            ValidatedHeaderStore::create(
                directory.path().join("unpublished.redb"),
                config(),
                anchor,
                &headers[..1]
            )
            .is_err()
        );
    }
    drop(store);
    for filler in fillers {
        std::fs::remove_file(filler).unwrap();
    }
    match ValidatedHeaderStore::open(&path, config(), anchor) {
        Ok(store) => {
            assert_eq!(store.anchor(), anchor);
            assert_eq!(store.protected_height().unwrap(), 0);
            let (first, tip) = store.bounds().unwrap();
            for height in first..=tip {
                assert_eq!(store.header(height).unwrap(), headers[height as usize]);
            }
            println!("{mode}: ENOSPC then exact committed chain {first}..={tip}");
        }
        Err(error) => {
            assert!(path.exists());
            println!("{mode}: ENOSPC then explicit refusal, no reinitialization: {error}");
        }
    }
}
