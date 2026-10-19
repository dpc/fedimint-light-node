use std::time::Duration;

use bitcoin::Network;
use bitcoin::consensus::deserialize;
use tokio::time::{Instant, advance, timeout};

use super::*;
use crate::peer_pool_config::PeerPoolConfig;
use crate::store_anchor::StoreAnchor;
use crate::store_config::StoreConfig;

#[tokio::test(start_paused = true)]
async fn coverage_reservations_survive_cancel_and_skip_useless_deadline_slices() {
    let headers: Vec<Header> = include_bytes!("../tests/data/mainnet-headers-0-2035.raw")
        .as_chunks::<80>()
        .0
        .iter()
        .take(2)
        .map(|bytes| deserialize(bytes).unwrap())
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let store = ValidatedHeaderStore::create(
        dir.path().join("chain"),
        StoreConfig {
            header_capacity: 4.try_into().unwrap(),
            cache_bytes: (1024 * 1024).try_into().unwrap(),
        },
        StoreAnchor {
            network: Network::Bitcoin,
            height: 0,
            hash: headers[0].block_hash(),
            height_one_hash: headers[1].block_hash(),
        },
        &headers[..1],
    )
    .unwrap();
    let mut sync = HeaderSync::new_with_mempool(
        store,
        SyncConfig {
            peers: vec!["127.0.0.1:1".parse().unwrap()],
            peer_pool: PeerPoolConfig::manual_only(),
            handshake_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            // Cancellation occurs during pacing, before any dial.
            retry_delay: Duration::from_secs(1),
            requests_per_visit: 1,
            candidate_headers: 4,
        },
        true,
    )
    .unwrap();
    let start = Instant::now();
    let window = sync.coverage_window;
    sync.cover_mempool_until(start + Duration::from_millis(249))
        .await
        .unwrap();
    assert_eq!(sync.coverage_bytes, 8 * 1024 * 1024);
    for expected in [4 * 1024 * 1024, 0] {
        assert!(
            timeout(Duration::from_millis(1), sync.cover_mempool())
                .await
                .is_err()
        );
        assert_eq!(sync.coverage_bytes, expected);
        assert_eq!(sync.coverage_window, window);
        assert_eq!(sync.pool.counts().0, 0);
    }
    // Repeated owner calls and reads cannot reset coverage accounting.
    for _ in 0..10 {
        sync.mempool_snapshot().unwrap();
        sync.cover_mempool().await.unwrap();
        assert_eq!(sync.coverage_bytes, 0);
        assert_eq!(sync.coverage_window, window);
    }
    advance(Duration::from_secs(60)).await;
    assert!(
        timeout(Duration::from_millis(1), sync.cover_mempool())
            .await
            .is_err()
    );
    assert_eq!(sync.coverage_bytes, 4 * 1024 * 1024);
    assert!(sync.coverage_window > window);
    sync.shutdown().await;
}
