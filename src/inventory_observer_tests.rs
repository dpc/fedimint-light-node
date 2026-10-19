use bitcoin::Network;
use bitcoin::consensus::serialize;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_blockdata::Inventory;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use super::*;

async fn receive(payload: Vec<u8>, corrupt: bool, inbox: Arc<Mutex<InventoryInbox>>) -> bool {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut server, _) = listener.accept().await.unwrap();
    let mut observer = InventoryObserver::new(inbox, 7);
    let mut checksum = sha256d::Hash::hash(&payload)[..4].to_vec();
    if corrupt {
        checksum[0] ^= 1;
    }
    let write = async { server.write_all(&payload).await.unwrap() };
    let read = async {
        observer
            .receive(&mut { client }, payload.len(), &checksum)
            .await
    };
    let (_, result) = tokio::join!(write, read);
    result.is_ok()
}

fn payload(items: Vec<Inventory>) -> Vec<u8> {
    serialize(&RawNetworkMessage::new(
        Network::Bitcoin.magic(),
        NetworkMessage::Inv(items),
    ))[24..]
        .to_vec()
}

#[tokio::test]
async fn checksum_commit_and_exact_txid_type() {
    let txid = Txid::from_byte_array([1; 32]);
    let inbox = InventoryInbox::new();
    let bytes = payload(vec![
        Inventory::Transaction(txid),
        Inventory::WitnessTransaction(txid),
        Inventory::WTx(bitcoin::Wtxid::from_byte_array([2; 32])),
    ]);
    assert!(!receive(bytes.clone(), true, inbox.clone()).await);
    assert!(inbox.lock().unwrap().queue.is_empty());
    assert!(receive(bytes, false, inbox.clone()).await);
    assert_eq!(
        inbox
            .lock()
            .unwrap()
            .queue
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![(txid, 7)]
    );
}

#[tokio::test]
async fn malformed_count_length_and_noncanonical_prefix_never_commit() {
    for bytes in [
        vec![],
        vec![253, 1, 0],
        vec![254, 0, 0, 0, 0],
        vec![1],
        vec![0, 0],
        vec![253, 0x51, 0xc3],
    ] {
        let inbox = InventoryInbox::new();
        assert!(!receive(bytes, false, inbox.clone()).await);
        assert!(inbox.lock().unwrap().queue.is_empty());
    }
}

#[tokio::test]
async fn bounded_staging_inbox_and_aggregate_storm_accounting() {
    let inbox = InventoryInbox::new();
    let bytes = payload(vec![Inventory::Transaction(Txid::all_zeros()); 2000]);
    for _ in 0..6 {
        assert!(receive(bytes.clone(), false, inbox.clone()).await);
    }
    {
        let state = inbox.lock().unwrap();
        assert_eq!(state.queue.len(), INBOX);
        assert_eq!(state.dropped, 12_000 - INBOX as u64);
    }
    let mut observer = InventoryObserver::new(inbox.clone(), 3);
    assert!(observer.reserve(2 * 1024 * 1024).is_ok());
    assert!(observer.reserve(1).is_err());
    let mut other = InventoryObserver::new(inbox.clone(), 4);
    inbox.lock().unwrap().bytes = 0;
    assert!(other.reserve(1).is_err());
}

#[tokio::test(start_paused = true)]
async fn rate_windows_refresh_without_refunding_failed_frames() {
    let inbox = InventoryInbox::new();
    let mut observer = InventoryObserver::new(inbox.clone(), 3);
    observer.reserve(2 * 1024 * 1024).unwrap();
    assert!(observer.reserve(1).is_err());
    tokio::time::advance(Duration::from_secs(60)).await;
    observer.reserve(2 * 1024 * 1024).unwrap();
    assert_eq!(inbox.lock().unwrap().bytes, 6 * 1024 * 1024);
}
