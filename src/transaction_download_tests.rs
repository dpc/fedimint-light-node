use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{Hash, sha256d};
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::message::RawNetworkMessage;
use bitcoin::{Block, Network};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;

fn transaction() -> Transaction {
    let block: Block = deserialize(include_bytes!("../tests/data/testnet-segwit.raw")).unwrap();
    let mut tx = block
        .txdata
        .into_iter()
        .find(|tx| tx.input.iter().any(|i| !i.witness.is_empty()))
        .unwrap();
    tx.input[0].previous_output = bitcoin::OutPoint::new(Txid::from_byte_array([7; 32]), 0);
    tx
}

fn wire(message: NetworkMessage) -> Vec<u8> {
    serialize(&RawNetworkMessage::new(Network::Bitcoin.magic(), message))
}

fn payload_wire(payload: &[u8]) -> Vec<u8> {
    let mut frame = wire(NetworkMessage::Tx(transaction()));
    frame.truncate(24);
    frame[16..20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    frame[20..24].copy_from_slice(&sha256d::Hash::hash(payload)[..4]);
    frame.extend_from_slice(payload);
    frame
}

async fn connection() -> (PeerConnection, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let client = TcpStream::connect(addr).await.unwrap();
    let (server, _) = listener.accept().await.unwrap();
    (
        PeerConnection {
            stream: client,
            addr,
            services: ServiceFlags::WITNESS,
            relay: true,
            learned: Vec::new(),
            requested_addresses: false,
            idle_window: Instant::now(),
            idle_bytes: 256 * 1024,
            idle_frames: 64,
            observer: None,
        },
        server,
    )
}

async fn exchange(frame: Vec<u8>, received: usize) -> Result<Option<Transaction>, SyncError> {
    let txid = transaction().compute_txid();
    let (mut peer, mut server) = connection().await;
    let serving = async {
        let mut request = [0; 61];
        server.read_exact(&mut request).await.unwrap();
        assert_eq!(
            deserialize::<RawNetworkMessage>(&request)
                .unwrap()
                .into_payload(),
            NetworkMessage::GetData(vec![Inventory::WitnessTransaction(txid)])
        );
        let _ = server.write_all(&frame).await;
        server.shutdown().await.unwrap();
    };
    let requesting = async { request(&mut peer, txid, &mut { received }, &mut 4096).await };
    let (result, ()) = tokio::join!(requesting, serving);
    result
}

#[tokio::test]
async fn witness_request_preserves_body_and_full_txid() {
    let tx = transaction();
    assert_ne!(
        tx.compute_txid().to_byte_array(),
        tx.compute_wtxid().to_byte_array()
    );
    assert_eq!(
        exchange(wire(NetworkMessage::Tx(tx.clone())), 1_000_000)
            .await
            .unwrap(),
        Some(tx.clone())
    );
    let mut wrong = tx;
    wrong.lock_time = bitcoin::absolute::LockTime::from_consensus(1);
    assert!(matches!(
        exchange(wire(NetworkMessage::Tx(wrong)), 1_000_000).await,
        Err(SyncError::Protocol("transaction identity"))
    ));
}

#[tokio::test]
async fn notfound_requires_requested_identity() {
    let txid = transaction().compute_txid();
    for inv in [
        Inventory::Transaction(txid),
        Inventory::WitnessTransaction(txid),
    ] {
        assert!(
            exchange(wire(NetworkMessage::NotFound(vec![inv])), 1000)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        exchange(
            wire(NetworkMessage::NotFound(vec![Inventory::Transaction(
                Txid::all_zeros()
            )])),
            1000
        )
        .await
        .is_err()
    );
    assert!(
        exchange(wire(NetworkMessage::NotFound(vec![])), 1000)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn malformed_envelopes_and_canonical_payloads_fail_closed() {
    let tx = transaction();
    let valid = wire(NetworkMessage::Tx(tx.clone()));
    let mut bad_magic = valid.clone();
    bad_magic[0] ^= 1;
    let mut bad_checksum = valid.clone();
    bad_checksum[20] ^= 1;
    let mut trailing = serialize(&tx);
    trailing.push(0);
    let mut nonminimal = serialize(&tx);
    // Segwit marker and flag precede the input count.
    assert_eq!(&nonminimal[4..6], &[0, 1]);
    let count = nonminimal[6];
    nonminimal.splice(6..7, [253, count, 0]);
    for frame in [
        bad_magic,
        bad_checksum,
        payload_wire(&trailing),
        payload_wire(&nonminimal),
        valid[..valid.len() - 1].to_vec(),
    ] {
        assert!(exchange(frame, 1_000_000).await.is_err());
    }
    assert!(exchange(valid, 24).await.is_err());
    let mut oversized = wire(NetworkMessage::Tx(tx));
    oversized.truncate(24);
    oversized[16..20].copy_from_slice(&400_001u32.to_le_bytes());
    assert!(matches!(
        exchange(oversized, 1_000_000).await,
        Err(SyncError::TransactionLimit)
    ));
}

#[tokio::test]
async fn unsolicited_transactions_are_not_admitted_by_header_reader() {
    let (mut peer, mut server) = connection().await;
    let frame = wire(NetworkMessage::Tx(transaction()));
    let mut received = 1_000_000;
    let (result, _) = tokio::join!(peer.receive(&mut received, None), server.write_all(&frame));
    assert!(matches!(result, Err(SyncError::UnsupportedCommand(_))));
}

#[tokio::test]
async fn control_frames_have_finite_frame_and_write_budgets() {
    let frames = wire(NetworkMessage::Pong(1)).repeat(32);
    assert!(matches!(
        exchange(frames, 1_000_000).await,
        Err(SyncError::Protocol("transaction response message budget"))
    ));
    let (mut peer, _) = connection().await;
    assert!(matches!(
        request(
            &mut peer,
            transaction().compute_txid(),
            &mut 1_000_000,
            &mut 60
        )
        .await,
        Err(SyncError::Protocol("sent-byte budget"))
    ));
}
