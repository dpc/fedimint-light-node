//! Opt-in release measurements, not process-wide resource guarantees.
//!
//! Run each ignored test in its own process; see docs/resource-exercise.md.
#![cfg(target_os = "linux")]
use std::io::Read;
use std::num::{NonZeroU64, NonZeroUsize};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Instant;

use bitcoin::block::Header;
use bitcoin::blockdata::constants::genesis_block;
use bitcoin::consensus::serialize;
use bitcoin::hashes::Hash;
use bitcoin::hex::DisplayHex;
use bitcoin::{Amount, Block, Network, OutPoint, ScriptBuf, TxOut, Txid, Witness};
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
#[ignore = "external bounded header capture required; see docs/resource-exercise.md"]
fn default_validated_store_rollover() {
    // Authenticate the capture separately using its manifest. This is a sizing
    // exercise rooted in explicitly trusted recent context, not a new trust path.
    let path = std::env::var("READINESS_HEADERS").expect("READINESS_HEADERS");
    let start: u64 = std::env::var("READINESS_START")
        .unwrap_or_else(|_| "810432".into())
        .parse()
        .unwrap();
    assert_eq!(start % 2016, 0);
    let expected_bytes = (2016 + 3 * 52_560) * 80;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .take(expected_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes.len(), expected_bytes);
    let headers: Vec<Header> = bytes
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect();
    drop(bytes);
    let anchor_height = start + 2015;
    let anchor = StoreAnchor {
        network: Network::Bitcoin,
        height: anchor_height,
        hash: headers[2015].block_hash(),
        height_one_hash: bitcoin::consensus::deserialize::<Header>(
            &include_bytes!("data/mainnet-headers-0-2035.raw")[80..160],
        )
        .unwrap()
        .block_hash(),
    };
    let settings = StoreConfig {
        header_capacity: NonZeroU64::new(52_560).unwrap(),
        cache_bytes: NonZeroUsize::new(8 * 1024 * 1024).unwrap(),
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default-validated.redb");
    let mut store =
        ValidatedHeaderStore::create(&path, settings, anchor, &headers[..2016]).unwrap();
    let started = Instant::now();
    let mut tip = anchor_height;
    for (cycle, window) in headers[2016..].chunks(52_560).enumerate() {
        for batch in window.chunks(144) {
            store.select_suffix(tip, batch, u64::MAX).unwrap();
            tip += batch.len() as u64;
        }
        assert_eq!(store.bounds().unwrap(), (tip + 1 - 52_560, tip));
        assert_eq!(store.header(tip).unwrap(), headers[(tip - start) as usize]);
        assert!(matches!(
            store.header(tip - 52_560),
            Err(StoreError::OutsideRetainedHistory)
        ));
        drop(store);
        inspect(&path, 52_560, true);
        let reopen = Instant::now();
        store = ValidatedHeaderStore::open(&path, settings, anchor).unwrap();
        assert_eq!(store.tip_context().unwrap().height(), tip);
        println!(
            "cycle={cycle} tip={tip} reopen_ms={} total_ms={} cache_bytes=8388608",
            reopen.elapsed().as_millis(),
            started.elapsed().as_millis()
        );
        rss();
    }
    // Compose real default state with admitted near-limit witness shapes and the
    // eight response copies allowed by the RPC contract. This is not a running
    // RPC server/pool saturation test; fixture memory remains in this process.
    for many_items in [false, true] {
        measure_witness_responses(many_items);
        assert_eq!(store.tip_context().unwrap().height(), tip);
    }
}

fn measure_witness_responses(many_items: bool) {
    let mut block = genesis_block(Network::Bitcoin);
    let mut transaction = block.txdata[0].clone();
    transaction.input[0].previous_output = OutPoint::new(Txid::from_byte_array([1; 32]), 0);
    let item_count = if many_items { 999_980 } else { 0 };
    let blob = vec![0u8; 3_990_000 - item_count];
    let mut items = vec![&[][..]; item_count];
    items.push(&blob);
    transaction.input[0].witness = Witness::from_slice(&items);
    drop(items);
    drop(blob);
    block.txdata.push(transaction);
    block.txdata[0].input[0].witness = Witness::from_slice(&[[0u8; 32]]);
    let commitment = Block::compute_witness_commitment(&block.witness_root().unwrap(), &[0; 32]);
    let mut script = vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
    script.extend_from_slice(commitment.as_byte_array());
    block.txdata[0].output.push(TxOut {
        value: Amount::ZERO,
        script_pubkey: ScriptBuf::from_bytes(script),
    });
    block.header.merkle_root = block.compute_merkle_root().unwrap();
    let requested = block.block_hash();
    let payload = serialize(&block);
    drop(block);
    let decoded = decode_block(
        &payload,
        requested,
        WitnessRules::Active,
        BlockDecodeLimits {
            max_payload_bytes: 4_000_000,
            max_elements: 1_000_000,
        },
    )
    .unwrap();
    let result = serde_json::Value::String(serialize(&decoded).to_lower_hex_string());
    let responses: Vec<_> = (0..8)
        .map(|_| {
            serde_json::to_vec(&serde_json::json!({"id": 1, "result": result, "error": null}))
                .unwrap()
        })
        .collect();
    println!(
        "composed_witness_items={} payload_bytes={} weight={} response_copies=8 response_bytes={}",
        item_count + 1,
        payload.len(),
        decoded.weight().to_wu(),
        responses.iter().map(Vec::len).sum::<usize>()
    );
    rss();
    std::hint::black_box((&decoded, &responses));
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
