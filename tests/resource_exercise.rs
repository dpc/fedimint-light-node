//! Opt-in release measurements, not process-wide resource guarantees.
//!
//! Run each ignored test in its own process; see docs/resource-exercise.md.
#![cfg(target_os = "linux")]
use std::num::{NonZeroU64, NonZeroUsize};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Instant;

use bitcoin::block::Header;
use bitcoin::blockdata::constants::genesis_block;
use bitcoin::consensus::serialize;
use bitcoin::{Amount, Block, Network, ScriptBuf, TxOut};
use fedimint_light_node::block_decode::{BlockDecodeLimits, decode_block};
use fedimint_light_node::block_integrity::WitnessRules;
use fedimint_light_node::header_store::HeaderStore;
use fedimint_light_node::store_anchor::StoreAnchor;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::store_error::StoreError;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

fn config(capacity: u64) -> StoreConfig {
    StoreConfig {
        header_capacity: NonZeroU64::new(capacity).unwrap(),
        cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
    }
}

fn rss() {
    // Linux reports process lifetime high-water RSS, including fixture creation,
    // test runner, redb and allocator overhead; this is not a library heap counter.
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    for line in status
        .lines()
        .filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:"))
    {
        println!("{line}");
    }
}

fn inspect(path: &Path, expected_rows: u64, contexts: bool) {
    let metadata = std::fs::metadata(path).unwrap();
    let db = Database::builder()
        .set_cache_size(1024 * 1024)
        .open(path)
        .unwrap();
    let read = db.begin_read().unwrap();
    let headers = read
        .open_table(TableDefinition::<u64, &[u8]>::new("headers"))
        .unwrap();
    assert_eq!(headers.len().unwrap(), expected_rows);
    let header_bytes: usize = headers
        .iter()
        .unwrap()
        .map(|row| row.unwrap().1.value().len())
        .sum();
    let context_bytes: usize = if contexts {
        let table = read
            .open_table(TableDefinition::<u64, &[u8]>::new("contexts"))
            .unwrap();
        assert_eq!(table.len().unwrap(), expected_rows);
        table
            .iter()
            .unwrap()
            .map(|row| row.unwrap().1.value().len())
            .sum()
    } else {
        0
    };
    println!(
        "rows={expected_rows} header_value_bytes={header_bytes} context_value_bytes={context_bytes} file_bytes={} allocated_bytes={}",
        metadata.len(),
        metadata.blocks() * 512
    );
}

#[test]
#[ignore = "explicit local release measurement; Linux /proc required"]
fn raw_store_rollover() {
    // Deliberately unmined, context-free rows. Never label this validated sizing.
    let capacity = 52_560;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw.redb");
    let mut parent = genesis_block(Network::Regtest).header;
    let anchor = StoreAnchor {
        network: Network::Regtest,
        height: 0,
        hash: parent.block_hash(),
        height_one_hash: Header {
            prev_blockhash: parent.block_hash(),
            nonce: parent.nonce.wrapping_add(1),
            ..parent
        }
        .block_hash(),
    };
    let started = Instant::now();
    let mut store = HeaderStore::create(&path, config(capacity), anchor, parent).unwrap();
    let mut tip = 0;
    // Fill once, then replace the complete live window three times. Each batch
    // resembles a day of arrivals, not a per-block durable commit workload.
    for cycle in 0..4 {
        let cycle_start = Instant::now();
        for _ in 0..365 {
            let batch: Vec<_> = (0..144)
                .map(|_| {
                    parent = Header {
                        prev_blockhash: parent.block_hash(),
                        nonce: parent.nonce.wrapping_add(1),
                        ..parent
                    };
                    parent
                })
                .collect();
            store.replace_suffix(tip, &batch).unwrap();
            tip += 144;
        }
        assert_eq!(store.bounds().unwrap(), (tip + 1 - capacity, tip));
        assert!(matches!(
            store.header(tip - capacity),
            Err(StoreError::OutsideRetainedHistory)
        ));
        drop(store);
        inspect(&path, capacity, false);
        let reopen = Instant::now();
        store = HeaderStore::open(&path, config(capacity), anchor).unwrap();
        assert_eq!(store.header(tip).unwrap(), parent);
        println!(
            "cycle={cycle} tip={tip} reopen_ms={} cycle_ms={}",
            reopen.elapsed().as_millis(),
            cycle_start.elapsed().as_millis()
        );
    }
    println!("total_ms={}", started.elapsed().as_millis());
    rss();
}

#[test]
#[ignore = "explicit local release measurement; Linux /proc required"]
fn validated_store_rollover() {
    // Real mainnet PoW and contextual admission, but only the available fixture.
    // Capacity 512 intentionally forces several complete window replacements.
    let headers: Vec<Header> = include_bytes!("data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect();
    let anchor = StoreAnchor {
        network: Network::Bitcoin,
        height: 0,
        hash: headers[0].block_hash(),
        height_one_hash: headers[1].block_hash(),
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("validated.redb");
    let started = Instant::now();
    let mut store =
        ValidatedHeaderStore::create(&path, config(512), anchor, &headers[..1]).unwrap();
    for (index, batch) in headers[1..].chunks(144).enumerate() {
        let parent = index as u64 * 144;
        store.select_suffix(parent, batch, u64::MAX).unwrap();
        let tip = parent + batch.len() as u64;
        let first = (tip + 1).saturating_sub(512);
        assert_eq!(store.bounds().unwrap(), (first, tip));
        drop(store);
        inspect(&path, (tip + 1).min(512), true);
        let reopen = Instant::now();
        store = ValidatedHeaderStore::open(&path, config(512), anchor).unwrap();
        assert_eq!(store.tip_context().unwrap().height(), tip);
        assert_eq!(store.header(tip).unwrap(), headers[tip as usize]);
        if first > 0 {
            assert!(matches!(
                store.header(first - 1),
                Err(StoreError::OutsideRetainedHistory)
            ));
        }
        println!("tip={tip} reopen_ms={}", reopen.elapsed().as_millis());
    }
    println!("total_ms={}", started.elapsed().as_millis());
    rss();
}

#[test]
#[ignore = "explicit local release measurement; Linux /proc required"]
fn large_body_decode() {
    // Near the legacy weight limit, generated locally rather than checked in.
    // Mechanically valid, not mined and not script/consensus validated. A single
    // large script stresses byte copies, NOT worst-case nested-element expansion.
    let mut block = genesis_block(Network::Bitcoin);
    block.txdata[0].output[0].script_pubkey = ScriptBuf::from_bytes(vec![0; 990_000]);
    measure_decode(block);
}

#[test]
#[ignore = "explicit local release measurement; Linux /proc required"]
fn many_output_body_decode() {
    // Contrast byte-heavy scripts with decoded per-element object overhead.
    // This is a deliberately extreme mechanical body, not observed chain data.
    let mut block = genesis_block(Network::Bitcoin);
    block.txdata[0].output = vec![
        TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new(),
        };
        100_000
    ];
    measure_decode(block);
}

fn measure_decode(mut block: Block) {
    block.header.merkle_root = block.compute_merkle_root().unwrap();
    let hash = block.block_hash();
    let outputs = block.txdata[0].output.len();
    let payload = serialize(&block);
    drop(block);
    let started = Instant::now();
    for _ in 0..100 {
        let decoded = decode_block(
            &payload,
            hash,
            WitnessRules::BeforeActivation,
            BlockDecodeLimits {
                max_payload_bytes: 4_000_000,
                max_elements: 200_000,
            },
        )
        .unwrap();
        assert_eq!(decoded.txdata[0].output.len(), outputs);
        // Only one returned body alive at a time; caller accumulation is excluded.
    }
    println!(
        "payload_bytes={} outputs={outputs} decodes=100 total_ms={}",
        payload.len(),
        started.elapsed().as_millis()
    );
    rss();
}
