use bitcoin::blockdata::constants::genesis_block;
use bitcoin::{Amount, Network, TxOut};

use super::*;

fn block(outputs: usize, tag: u32) -> Block {
    let mut block = genesis_block(Network::Bitcoin);
    let tx = &mut block.txdata[0];
    tx.lock_time = bitcoin::absolute::LockTime::from_consensus(tag);
    tx.output = vec![
        TxOut {
            value: Amount::from_sat(1),
            script_pubkey: bitcoin::ScriptBuf::new(),
        };
        outputs
    ];
    block
}

#[test]
fn fixed_capacity_oldest_eviction_and_impossible_index() {
    let a = block(100_000, 1);
    let b = block(100_000, 2);
    let c = block(100_000, 3);
    let mut cache = OutputAmountCache::new();
    cache.ingest(1, &a);
    cache.ingest(2, &b);
    assert_eq!(cache.len(), 200_000);
    let a0 = OutPoint {
        txid: a.txdata[0].compute_txid(),
        vout: 0,
    };
    assert_eq!(cache.get(a0), Ok(Some((1, 1))));
    assert_eq!(
        cache.get(OutPoint {
            vout: 100_000,
            ..a0
        }),
        Err(())
    );
    cache.ingest(3, &c);
    assert_eq!(cache.len(), 200_000);
    assert_eq!(cache.evicted, 100_000);
    assert_eq!(cache.get(a0), Ok(None));
    let allocated = cache.allocated_bytes();
    // Reversed coverage order cannot displace newer outputs with older ones.
    cache.ingest(0, &a);
    assert_eq!(cache.len(), 200_000);
    assert_eq!(cache.evicted, 200_000);
    assert_eq!(cache.get(a0), Ok(None));
    assert_eq!(cache.allocated_bytes(), allocated);
    cache.retain(|height| height == 3);
    assert_eq!(cache.len(), 100_000);
    assert_eq!(cache.evicted, 300_000);
    cache.ingest(4, &block(MAX_OUTPUTS + 1, 4));
    assert_eq!(cache.len(), 100_000);
    assert_eq!(cache.allocated_bytes(), allocated);
}
