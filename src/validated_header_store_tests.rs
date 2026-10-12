use std::num::{NonZeroU64, NonZeroUsize};

use bitcoin::Network;
use bitcoin::block::Header;

use super::ValidatedHeaderStore;
use crate::header_context::HeaderContext;
use crate::store_anchor::StoreAnchor;
use crate::store_config::StoreConfig;

#[test]
fn shorter_selected_suffix_removes_obsolete_validation_checkpoints() {
    let headers: Vec<Header> = include_bytes!("../tests/data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain");
    let config = StoreConfig {
        header_capacity: NonZeroU64::new(8).unwrap(),
        cache_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
    };
    let anchor = StoreAnchor {
        network: Network::Bitcoin,
        height: 0,
        hash: headers[0].block_hash(),
        height_one_hash: headers[1].block_hash(),
    };
    let mut store = ValidatedHeaderStore::create(&path, config, anchor, &headers[..1]).unwrap();
    store.select_suffix(0, &headers[1..4], u64::MAX).unwrap();
    let mut context = HeaderContext::from_trusted_history(anchor, &headers[..1]).unwrap();
    let contexts: Vec<_> = headers[1..3]
        .iter()
        .map(|header| {
            context = context.validate_next(*header, u64::MAX).unwrap();
            context.clone()
        })
        .collect();
    // Exercise the atomic writer after selection, independently of the work
    // comparison (covered by HeaderContext tests). A higher-work branch may be
    // shorter. No easy-target or PoW bypass is added to the production validator.
    // Reusing a real prefix isolates deletion of *both* old rows and checkpoints.
    store
        .store
        .replace_inner(0, &headers[1..3], Some(&contexts))
        .unwrap();
    drop(store);
    let mut reopened = ValidatedHeaderStore::open(&path, config, anchor).unwrap();
    assert_eq!(reopened.bounds().unwrap(), (0, 2));
    assert_eq!(reopened.tip_context().unwrap().tip(), headers[2]);
    // Opening checks that there are no stale checkpoints above the selected tip.
    reopened.select_suffix(2, &headers[3..5], u64::MAX).unwrap();
    assert_eq!(reopened.bounds().unwrap(), (0, 4));
}
