use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;

use bitcoin::Network;
use bitcoin::block::Header;
use bitcoin::blockdata::constants::genesis_block;
use bitcoin::hashes::Hash;
use fedimint_light_node::header_store::HeaderStore;
use fedimint_light_node::store_anchor::StoreAnchor;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::store_error::StoreError;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

const META: TableDefinition<u8, &[u8]> = TableDefinition::new("metadata");
const HEADERS: TableDefinition<u64, &[u8]> = TableDefinition::new("headers");

fn config(capacity: u64) -> StoreConfig {
    StoreConfig {
        header_capacity: NonZeroU64::new(capacity).unwrap(),
        cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
    }
}

fn anchor(height: u64) -> (StoreAnchor, Header) {
    // Deliberately synthetic heights and unmined descendants demonstrate that
    // HeaderStore is not a Bitcoin header validator.
    let header = genesis_block(Network::Regtest).header;
    (
        StoreAnchor {
            network: Network::Regtest,
            height_one_hash: successor(header, 1).block_hash(),
            height,
            hash: header.block_hash(),
        },
        header,
    )
}

fn successor(parent: Header, nonce: u32) -> Header {
    Header {
        prev_blockhash: parent.block_hash(),
        nonce,
        ..parent
    }
}

fn descendants(mut parent: Header, count: u32) -> Vec<Header> {
    (1..=count)
        .map(|nonce| {
            parent = successor(parent, nonce);
            parent
        })
        .collect()
}

#[test]
fn prune_reopen_and_pin_identity_independently_of_headers() {
    // REQ-bounded-history: retention follows chain advancement, not consumers.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.redb");
    let (anchor, initial) = anchor(100);
    let config = config(3);
    let mut store = HeaderStore::create(&path, config, anchor, initial).unwrap();
    assert_eq!(store.bounds().unwrap(), (100, 100));
    let chain = descendants(initial, 3);
    store.replace_suffix(100, &chain).unwrap();
    assert_eq!(store.bounds().unwrap(), (101, 103));
    assert!(matches!(
        store.header(100),
        Err(StoreError::OutsideRetainedHistory)
    ));
    assert!(matches!(store.header(104), Err(StoreError::BeyondTip)));
    assert_eq!(store.anchor(), anchor);
    drop(store);

    let mut reopened_config = config;
    reopened_config.cache_bytes = NonZeroUsize::new(2 * 1024 * 1024).unwrap();
    let mut store = HeaderStore::open(&path, reopened_config, anchor).unwrap();
    assert_eq!(store.anchor(), anchor);
    assert_eq!(store.bounds().unwrap(), (101, 103));
    for (offset, header) in chain.iter().enumerate() {
        assert_eq!(store.header(101 + offset as u64).unwrap(), *header);
    }
    let more = descendants(chain[2], 3);
    store.replace_suffix(103, &more).unwrap();
    assert_eq!(store.bounds().unwrap(), (104, 106));
    drop(store);
    let database = Database::open(&path).unwrap();
    let read = database.begin_read().unwrap();
    let headers = read.open_table(HEADERS).unwrap();
    // Check actual persisted rows, not only the public range filter.
    let heights: Vec<_> = headers
        .iter()
        .unwrap()
        .map(|entry| entry.unwrap().0.value())
        .collect();
    assert_eq!(heights, [104, 105, 106]);
    assert!(read.open_table(META).unwrap().get(0).unwrap().is_some());
}

#[test]
fn suffix_replacement_and_truncation_never_restore_pruned_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.redb");
    let (anchor, initial) = anchor(10);
    let config = config(3);
    let mut store = HeaderStore::create(&path, config, anchor, initial).unwrap();
    let chain = descendants(initial, 3);
    store.replace_suffix(10, &chain).unwrap();
    let fork = successor(chain[0], 42);
    store.replace_suffix(11, &[fork]).unwrap();
    assert_eq!(store.bounds().unwrap(), (11, 12));
    assert_eq!(store.header(12).unwrap(), fork);
    assert!(matches!(store.header(13), Err(StoreError::BeyondTip)));
    store.replace_suffix(11, &[]).unwrap();
    assert_eq!(store.bounds().unwrap(), (11, 11));
    assert!(matches!(
        store.header(10),
        Err(StoreError::OutsideRetainedHistory)
    ));
    drop(store);
    let store = HeaderStore::open(&path, config, anchor).unwrap();
    assert_eq!(store.bounds().unwrap(), (11, 11));
    assert_eq!(store.header(11).unwrap(), chain[0]);
}

#[test]
fn late_invalid_header_rolls_back_prior_writes_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.redb");
    let (anchor, initial) = anchor(10);
    let config = config(3);
    let mut store = HeaderStore::create(&path, config, anchor, initial).unwrap();
    let chain = descendants(initial, 3);
    store.replace_suffix(10, &chain).unwrap();
    let fork = successor(chain[0], 42);
    // The first write replaces height 12, then the second fails linkage.
    assert!(matches!(
        store.replace_suffix(11, &[fork, chain[2]]),
        Err(StoreError::InvalidUpdate("header linkage mismatch"))
    ));
    assert_eq!(store.bounds().unwrap(), (11, 13));
    assert_eq!(store.header(12).unwrap(), chain[1]);
    drop(store);
    let store = HeaderStore::open(&path, config, anchor).unwrap();
    assert_eq!(store.bounds().unwrap(), (11, 13));
    for (offset, expected) in chain.iter().enumerate() {
        assert_eq!(store.header(11 + offset as u64).unwrap(), *expected);
    }
}

#[test]
fn reject_oversized_overflowing_and_out_of_range_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.redb");
    let (anchor, initial) = anchor(u64::MAX - 1);
    let config = config(1);
    let mut store = HeaderStore::create(&path, config, anchor, initial).unwrap();
    let chain = descendants(initial, 2);
    assert!(matches!(
        store.replace_suffix(anchor.height, &chain),
        Err(StoreError::InvalidUpdate("batch exceeds header capacity"))
    ));
    assert!(matches!(
        store.replace_suffix(anchor.height - 1, &[]),
        Err(StoreError::RecoveryRequired)
    ));
    assert!(matches!(
        store.replace_suffix(u64::MAX, &[]),
        Err(StoreError::RecoveryRequired)
    ));
    store.replace_suffix(anchor.height, &chain[..1]).unwrap();
    assert_eq!(store.bounds().unwrap(), (u64::MAX, u64::MAX));
    assert!(matches!(
        store.replace_suffix(u64::MAX, &chain[1..]),
        Err(StoreError::InvalidUpdate("height overflow"))
    ));
    assert!(matches!(
        store.replace_suffix(anchor.height, &[]),
        Err(StoreError::RecoveryRequired)
    ));
    drop(store);
    let store = HeaderStore::open(&path, config, anchor).unwrap();
    assert_eq!(store.header(u64::MAX).unwrap(), chain[0]);
    assert_eq!(store.anchor(), anchor);
}

#[test]
fn reject_wrong_anchor_identity_capacity_and_reinitialization() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.redb");
    let (anchor, initial) = anchor(100);
    let config = config(3);
    assert!(matches!(
        HeaderStore::create(&path, config, anchor, successor(initial, 3)),
        Err(StoreError::InvalidUpdate(_))
    ));
    assert!(!path.exists());
    assert!(HeaderStore::open(&path, config, anchor).is_err());
    assert!(!path.exists());
    drop(HeaderStore::create(&path, config, anchor, initial).unwrap());
    assert!(matches!(
        HeaderStore::create(&path, config, anchor, initial),
        Err(StoreError::Io(_))
    ));
    let alternatives = [
        StoreAnchor {
            network: Network::Bitcoin,
            ..anchor
        },
        StoreAnchor {
            height: 99,
            ..anchor
        },
        StoreAnchor {
            hash: successor(initial, 1).block_hash(),
            ..anchor
        },
        StoreAnchor {
            height_one_hash: anchor.hash,
            ..anchor
        },
    ];
    for wrong in alternatives {
        assert!(matches!(
            HeaderStore::open(&path, config, wrong),
            Err(StoreError::ConfigurationMismatch)
        ));
    }
    let mut wrong_config = config;
    wrong_config.header_capacity = NonZeroU64::new(2).unwrap();
    assert!(matches!(
        HeaderStore::open(&path, wrong_config, anchor),
        Err(StoreError::ConfigurationMismatch)
    ));
    assert_eq!(
        HeaderStore::open(&path, config, anchor).unwrap().anchor(),
        anchor
    );
}

#[test]
fn height_one_identity_is_checked_when_available() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.redb");
    let (mut anchor, initial) = anchor(0);
    let config = config(2);
    let mut store = HeaderStore::create(&path, config, anchor, initial).unwrap();
    assert!(matches!(
        store.replace_suffix(0, &[successor(initial, 2)]),
        Err(StoreError::InvalidUpdate("height-one identity mismatch"))
    ));
    store.replace_suffix(0, &[successor(initial, 1)]).unwrap();
    drop(store);
    anchor.height = 1;
    assert!(matches!(
        HeaderStore::create(dir.path().join("bad"), config, anchor, initial),
        Err(StoreError::InvalidUpdate("height-one identity mismatch"))
    ));
}

fn mutate_metadata(path: &Path, mutate: impl FnOnce(&mut Vec<u8>)) {
    let db = Database::open(path).unwrap();
    let write = db.begin_write().unwrap();
    {
        let mut table = write.open_table(META).unwrap();
        let mut bytes = table.get(0).unwrap().unwrap().value().to_vec();
        mutate(&mut bytes);
        table.insert(0, bytes.as_slice()).unwrap();
    }
    write.commit().unwrap();
}

#[test]
fn reject_incompatible_corrupt_trailing_and_oversized_metadata_without_reset() {
    for mutation in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("headers.redb");
        let (anchor, initial) = anchor(100);
        let config = config(3);
        drop(HeaderStore::create(&path, config, anchor, initial).unwrap());
        mutate_metadata(&path, |bytes| match mutation {
            0 => bytes[8..12].copy_from_slice(&1u32.to_le_bytes()),
            1 => bytes[0] ^= 1,
            2 => bytes.truncate(15),
            3 => bytes.push(0),
            4 => bytes.resize(1024, 0),
            _ => unreachable!(),
        });
        for _ in 0..2 {
            let error = HeaderStore::open(&path, config, anchor).err().unwrap();
            if mutation == 0 {
                assert!(matches!(error, StoreError::UnsupportedSchema(1)));
            } else {
                assert!(matches!(error, StoreError::Corrupt(_)), "{error}");
            }
        }
    }
}

#[test]
fn reject_missing_extra_and_inconsistent_header_records() {
    for mutation in 0..7 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("headers.redb");
        let (anchor, initial) = anchor(100);
        let config = config(3);
        let mut store = HeaderStore::create(&path, config, anchor, initial).unwrap();
        let chain = descendants(initial, 2);
        store.replace_suffix(100, &chain).unwrap();
        drop(store);
        {
            let db = Database::open(&path).unwrap();
            let write = db.begin_write().unwrap();
            {
                let mut table = write.open_table(HEADERS).unwrap();
                let mut bytes = table.get(102).unwrap().unwrap().value().to_vec();
                match mutation {
                    0 => {
                        table.remove(101).unwrap();
                    }
                    1 => {
                        table.insert(103, bytes.as_slice()).unwrap();
                    }
                    2 => {
                        bytes.push(0);
                        table.insert(102, bytes.as_slice()).unwrap();
                    }
                    3 => {
                        bytes[0..4].copy_from_slice(&1u32.to_le_bytes());
                        table.insert(102, bytes.as_slice()).unwrap();
                    }
                    4 => {
                        // Header previous-block hash starts after version.
                        bytes[8] ^= 1;
                        table.insert(102, bytes.as_slice()).unwrap();
                    }
                    5 => {
                        // Nonce alteration preserves linkage but changes tip hash.
                        bytes[83] ^= 1;
                        table.insert(102, bytes.as_slice()).unwrap();
                    }
                    6 => {
                        table.remove(101).unwrap();
                        table.insert(99, bytes.as_slice()).unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            write.commit().unwrap();
        }
        assert!(matches!(
            HeaderStore::open(&path, config, anchor),
            Err(StoreError::Corrupt(_) | StoreError::UnsupportedSchema(_))
        ));
    }
}

#[test]
fn missing_metadata_and_foreign_files_are_not_initialized() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.redb");
    let (anchor, _) = anchor(100);
    let config = config(3);
    drop(Database::create(&path).unwrap());
    assert!(matches!(
        HeaderStore::open(&path, config, anchor),
        Err(StoreError::Table(_))
    ));
    let foreign = dir.path().join("foreign");
    std::fs::write(&foreign, b"not a database").unwrap();
    assert!(HeaderStore::open(&foreign, config, anchor).is_err());
    assert_eq!(std::fs::read(foreign).unwrap(), b"not a database");
}

#[test]
fn reject_impossible_bounds_and_unexpected_tables() {
    for mutation in 0..4 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("headers.redb");
        let (anchor, initial) = anchor(0);
        let config = config(3);
        drop(HeaderStore::create(&path, config, anchor, initial).unwrap());
        match mutation {
            0 => mutate_metadata(&path, |bytes| {
                // first > tip
                bytes[96..104].copy_from_slice(&1u64.to_le_bytes());
            }),
            1 => mutate_metadata(&path, |bytes| {
                // The inclusive range 0..=u64::MAX overflows its count.
                bytes[104..112].copy_from_slice(&u64::MAX.to_le_bytes());
            }),
            2 => mutate_metadata(&path, |bytes| {
                // In-bounds metadata still must match the actual rows.
                bytes[104..112].copy_from_slice(&2u64.to_le_bytes());
            }),
            3 => {
                let db = Database::open(&path).unwrap();
                let write = db.begin_write().unwrap();
                write
                    .open_table(TableDefinition::<u8, u8>::new("unknown"))
                    .unwrap();
                write.commit().unwrap();
            }
            _ => unreachable!(),
        }
        assert!(matches!(
            HeaderStore::open(&path, config, anchor),
            Err(StoreError::Corrupt(_))
        ));
    }
}

#[test]
fn schema_v2_bytes_do_not_depend_on_rust_bitcoin_serde_layout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.redb");
    let (anchor, initial) = anchor(100);
    drop(HeaderStore::create(&path, config(3), anchor, initial).unwrap());
    let db = Database::open(&path).unwrap();
    let read = db.begin_read().unwrap();
    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"FLNSTORE");
    metadata.extend_from_slice(&2u32.to_le_bytes());
    metadata.extend_from_slice(&anchor.network.magic().to_bytes());
    metadata.extend_from_slice(anchor.height_one_hash.as_byte_array());
    metadata.extend_from_slice(&100u64.to_le_bytes());
    metadata.extend_from_slice(anchor.hash.as_byte_array());
    metadata.extend_from_slice(&3u64.to_le_bytes());
    metadata.extend_from_slice(&100u64.to_le_bytes());
    metadata.extend_from_slice(&100u64.to_le_bytes());
    metadata.extend_from_slice(anchor.hash.as_byte_array());
    metadata.push(0); // Raw storage, not contextual admission.
    metadata.extend_from_slice(&100u64.to_le_bytes()); // Initial protection floor.
    assert_eq!(metadata.len(), 153);
    assert_eq!(
        read.open_table(META)
            .unwrap()
            .get(0)
            .unwrap()
            .unwrap()
            .value(),
        metadata.as_slice()
    );
    let mut header = 2u32.to_le_bytes().to_vec();
    header.extend_from_slice(&bitcoin::consensus::serialize(&initial));
    assert_eq!(header.len(), 84);
    assert_eq!(
        read.open_table(HEADERS)
            .unwrap()
            .get(100)
            .unwrap()
            .unwrap()
            .value(),
        header.as_slice()
    );
}

#[test]
fn long_update_sequence_matches_bounded_in_memory_model() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("headers.redb");
    let (anchor, initial) = anchor(100);
    let config = config(5);
    let mut store = HeaderStore::create(&path, config, anchor, initial).unwrap();
    let mut model = vec![(100, initial)];
    for round in 0..80 {
        // Exercise append, shorter/longer fork replacement and pure truncation.
        let parent_index = if round % 3 == 0 { 0 } else { model.len() - 1 };
        let (parent_height, parent_header) = model[parent_index];
        let count = round % 6;
        let mut replacement = descendants(parent_header, count);
        if let Some(first) = replacement.first_mut() {
            first.nonce += round * 100;
            for index in 1..replacement.len() {
                replacement[index].prev_blockhash = replacement[index - 1].block_hash();
            }
        }
        store.replace_suffix(parent_height, &replacement).unwrap();
        model.truncate(parent_index + 1);
        model.extend(
            replacement
                .iter()
                .enumerate()
                .map(|(offset, header)| (parent_height + offset as u64 + 1, *header)),
        );
        if model.len() > 5 {
            model.drain(..model.len() - 5);
        }
        assert_eq!(
            store.bounds().unwrap(),
            (model[0].0, model.last().unwrap().0)
        );
        for (height, header) in &model {
            assert_eq!(store.header(*height).unwrap(), *header);
        }
        if round % 10 == 0 {
            drop(store);
            store = HeaderStore::open(&path, config, anchor).unwrap();
        }
    }
}
