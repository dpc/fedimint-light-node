use std::num::{NonZeroU64, NonZeroUsize};

use bitcoin::Network;
use bitcoin::block::Header;
use fedimint_light_node::header_context::HeaderContext;
use fedimint_light_node::header_error::HeaderError;
use fedimint_light_node::header_store::HeaderStore;
use fedimint_light_node::store_anchor::StoreAnchor;
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::store_error::StoreError;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

const CONTEXTS: TableDefinition<u64, &[u8]> = TableDefinition::new("contexts");
const META: TableDefinition<u8, &[u8]> = TableDefinition::new("metadata");

fn headers() -> Vec<Header> {
    include_bytes!("data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect()
}

fn anchor(headers: &[Header], height: usize) -> StoreAnchor {
    StoreAnchor {
        network: Network::Bitcoin,
        height: height as u64,
        hash: headers[height].block_hash(),
        height_one_hash: headers[1].block_hash(),
    }
}

fn config(capacity: u64) -> StoreConfig {
    StoreConfig {
        header_capacity: NonZeroU64::new(capacity).unwrap(),
        cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
    }
}

#[test]
fn prune_to_one_reopen_and_validate_across_retarget() {
    let headers = headers();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain.redb");
    let anchor = anchor(&headers, 2014);
    let mut store =
        ValidatedHeaderStore::create(&path, config(1), anchor, &headers[..2015]).unwrap();
    for height in 2015..=2028 {
        store
            .select_suffix(
                height - 1,
                &headers[height as usize..=height as usize],
                u64::MAX,
            )
            .unwrap();
        assert_eq!(store.bounds().unwrap(), (height, height));
        drop(store);
        store = ValidatedHeaderStore::open(&path, config(1), anchor).unwrap();
        assert_eq!(store.tip_context().unwrap().height(), height);
        assert!(matches!(
            store.header(height - 1),
            Err(StoreError::OutsideRetainedHistory)
        ));
    }
    let expected = HeaderContext::from_trusted_history(anchor, &headers[..2015]).unwrap();
    let expected = headers[2015..=2028]
        .iter()
        .fold(expected, |context, header| {
            context.validate_next(*header, u64::MAX).unwrap()
        });
    assert_eq!(
        expected.relative_work(),
        store.tip_context().unwrap().relative_work()
    );
    assert_eq!(store.protected_height().unwrap(), 2014);
    assert!(matches!(
        store.select_suffix(2014, &[], u64::MAX),
        Err(HeaderError::RecoveryRequired)
    ));
    drop(store);
    let db = Database::open(&path).unwrap();
    let read = db.begin_read().unwrap();
    let contexts = read.open_table(CONTEXTS).unwrap();
    assert_eq!(contexts.len().unwrap(), 1);
    assert!(contexts.get(2028).unwrap().unwrap().value().len() <= 256);
}

#[test]
fn candidate_selection_is_atomic_bounded_and_strictly_more_work() {
    let headers = headers();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain.redb");
    let anchor = anchor(&headers, 2008);
    let mut store =
        ValidatedHeaderStore::create(&path, config(24), anchor, &headers[..2009]).unwrap();
    store
        .select_suffix(2008, &headers[2009..=2017], u64::MAX)
        .unwrap();
    let before = store.tip_context().unwrap().relative_work();
    // Replay a supported parent's suffix: ties never switch the chain.
    assert!(matches!(
        store.select_suffix(2014, &headers[2015..=2017], u64::MAX),
        Err(HeaderError::InsufficientWork)
    ));
    assert!(matches!(
        store.select_suffix(2014, &headers[2015..=2016], u64::MAX),
        Err(HeaderError::InsufficientWork)
    ));
    assert!(matches!(
        store.select_suffix(2014, &[], u64::MAX),
        Err(HeaderError::InsufficientWork)
    ));
    let mut broken = headers[2015..=2019].to_vec();
    broken.last_mut().unwrap().nonce ^= 1;
    assert!(matches!(
        store.select_suffix(2014, &broken, u64::MAX),
        Err(HeaderError::ProofOfWork(_))
    ));
    assert_eq!(store.bounds().unwrap(), (2008, 2017));
    assert_eq!(before, store.tip_context().unwrap().relative_work());
    assert!(matches!(
        store.select_suffix(2007, &[], u64::MAX),
        Err(HeaderError::RecoveryRequired)
    ));
    assert!(matches!(
        store.select_suffix(2018, &[], u64::MAX),
        Err(HeaderError::RecoveryRequired)
    ));
    assert!(store.select_suffix(2017, &headers[..25], u64::MAX).is_err());
    // A greater-work candidate rooted before the retarget is admitted.
    store
        .select_suffix(2014, &headers[2015..=2019], u64::MAX)
        .unwrap();
    drop(store);
    let store = ValidatedHeaderStore::open(&path, config(24), anchor).unwrap();
    assert_eq!(store.bounds().unwrap(), (2008, 2019));
    assert_eq!(store.header(2019).unwrap(), headers[2019]);
}

#[test]
fn finality_floor_survives_restart_and_pruning_and_cannot_regress() {
    let headers = headers();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain.redb");
    let anchor = anchor(&headers, 0);
    let mut store = ValidatedHeaderStore::create(&path, config(4), anchor, &headers[..1]).unwrap();
    store.select_suffix(0, &headers[1..=3], u64::MAX).unwrap();
    assert!(store.protect_through(2, headers[3].block_hash()).is_err());
    assert_eq!(store.protected_height().unwrap(), 0);
    store.protect_through(2, headers[2].block_hash()).unwrap();
    assert!(store.protect_through(1, headers[1].block_hash()).is_err());
    drop(store);
    let mut store = ValidatedHeaderStore::open(&path, config(4), anchor).unwrap();
    assert_eq!(store.protected_height().unwrap(), 2);
    assert!(matches!(
        store.select_suffix(1, &headers[2..=5], u64::MAX),
        Err(HeaderError::RecoveryRequired)
    ));
    store.select_suffix(2, &headers[3..=6], u64::MAX).unwrap();
    assert_eq!(store.bounds().unwrap(), (3, 6));
    assert!(matches!(
        store.protect_through(2, headers[2].block_hash()),
        Err(StoreError::OutsideRetainedHistory)
    ));
    drop(store);
    let store = ValidatedHeaderStore::open(&path, config(4), anchor).unwrap();
    assert_eq!(store.protected_height().unwrap(), 2);
}

#[test]
fn raw_and_validated_modes_cannot_promote_or_bypass_each_other() {
    let headers = headers();
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw.redb");
    let validated = dir.path().join("validated.redb");
    let anchor = anchor(&headers, 0);
    drop(HeaderStore::create(&raw, config(4), anchor, headers[0]).unwrap());
    assert!(matches!(
        ValidatedHeaderStore::open(&raw, config(4), anchor),
        Err(HeaderError::Store(StoreError::ConfigurationMismatch))
    ));
    drop(ValidatedHeaderStore::create(&validated, config(4), anchor, &headers[..1]).unwrap());
    assert!(matches!(
        HeaderStore::open(&validated, config(4), anchor),
        Err(StoreError::ConfigurationMismatch)
    ));
    let mut unsupported = anchor;
    unsupported.network = Network::Regtest;
    assert!(matches!(
        ValidatedHeaderStore::open(&validated, config(4), unsupported),
        Err(HeaderError::UnsupportedNetwork)
    ));
}

#[test]
fn damaged_checkpoint_or_old_schema_fails_without_repair() {
    let headers = headers();
    for mutation in 0..7 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.redb");
        let anchor = anchor(&headers, 0);
        let mut store =
            ValidatedHeaderStore::create(&path, config(4), anchor, &headers[..1]).unwrap();
        store.select_suffix(0, &headers[1..=3], u64::MAX).unwrap();
        drop(store);
        let db = Database::open(&path).unwrap();
        let write = db.begin_write().unwrap();
        {
            let mut contexts = write.open_table(CONTEXTS).unwrap();
            let mut bytes = contexts.get(2).unwrap().unwrap().value().to_vec();
            match mutation {
                0 => {
                    contexts.remove(2).unwrap();
                }
                1 => {
                    bytes.push(0);
                    contexts.insert(2, bytes.as_slice()).unwrap();
                }
                2 => {
                    bytes.resize(1024, 0);
                    contexts.insert(2, bytes.as_slice()).unwrap();
                }
                3 => {
                    bytes[0] ^= 1;
                    contexts.insert(2, bytes.as_slice()).unwrap();
                }
                4 => {
                    *bytes.last_mut().unwrap() ^= 1;
                    contexts.insert(2, bytes.as_slice()).unwrap();
                }
                5 => {
                    contexts.insert(10, bytes.as_slice()).unwrap();
                }
                6 => {
                    let mut metadata = write.open_table(META).unwrap();
                    let mut bytes = metadata.get(0).unwrap().unwrap().value().to_vec();
                    bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
                    metadata.insert(0, bytes.as_slice()).unwrap();
                }
                _ => unreachable!(),
            }
        }
        write.commit().unwrap();
        drop(db);
        for _ in 0..2 {
            assert!(ValidatedHeaderStore::open(&path, config(4), anchor).is_err());
        }
    }
}

#[test]
fn actual_divergent_mined_fork_replaces_suffix_but_not_protected_history() {
    let mainnet: Vec<Header> = include_bytes!("data/mainnet-headers-477792-478562.raw")
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect();
    let fork: Vec<Header> = include_bytes!("data/bch-headers-478559-478562.raw")
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect();
    let genesis_headers = headers();
    let anchor = StoreAnchor {
        network: Network::Bitcoin,
        height: 478_558,
        hash: mainnet[766].block_hash(),
        height_one_hash: genesis_headers[1].block_hash(),
    };
    assert_ne!(mainnet[767].block_hash(), fork[0].block_hash());
    assert_eq!(mainnet[767].prev_blockhash, fork[0].prev_blockhash);
    for protected in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.redb");
        let mut store =
            ValidatedHeaderStore::create(&path, config(5), anchor, &mainnet[..767]).unwrap();
        store
            .select_suffix(anchor.height, &mainnet[767..769], u64::MAX)
            .unwrap();
        // Both real branches have equal per-block difficulty; ties stay put.
        assert!(matches!(
            store.select_suffix(anchor.height, &fork[..2], u64::MAX),
            Err(HeaderError::InsufficientWork)
        ));
        if protected {
            store
                .protect_through(478_559, mainnet[767].block_hash())
                .unwrap();
        }
        drop(store);
        let mut store = ValidatedHeaderStore::open(&path, config(5), anchor).unwrap();
        if protected {
            assert!(matches!(
                store.select_suffix(anchor.height, &fork[..3], u64::MAX),
                Err(HeaderError::RecoveryRequired)
            ));
            assert_eq!(store.header(478_559).unwrap(), mainnet[767]);
        } else {
            // A lower-work continuation must not win; a longer fork can.
            assert!(matches!(
                store.select_suffix(anchor.height, &fork[..1], u64::MAX),
                Err(HeaderError::InsufficientWork)
            ));
            store
                .select_suffix(anchor.height, &fork[..3], u64::MAX)
                .unwrap();
            assert_eq!(store.header(478_559).unwrap(), fork[0]);
            assert_eq!(store.header(478_561).unwrap(), fork[2]);
            drop(store);
            let mut store = ValidatedHeaderStore::open(&path, config(5), anchor).unwrap();
            store.select_suffix(478_561, &fork[3..4], u64::MAX).unwrap();
            assert_eq!(store.header(478_562).unwrap(), fork[3]);
        }
    }
}
