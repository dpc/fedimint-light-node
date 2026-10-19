use bitcoin::consensus::{deserialize, serialize};
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_blockdata::Inventory;
use bitcoin::{Block, Network, Witness};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{request_block, witness_rules};
use crate::block_decode::BlockDecodeLimits;
use crate::block_download_config::BlockDownloadConfig;
use crate::block_download_error::BlockPeerError;
use crate::block_integrity::WitnessRules;

fn config() -> BlockDownloadConfig {
    BlockDownloadConfig {
        max_attempts: 1,
        total_timeout: std::time::Duration::from_secs(2),
        visit_timeout: std::time::Duration::from_secs(1),
        max_received_bytes: 10_000,
        decode: BlockDecodeLimits {
            max_payload_bytes: 5_000,
            max_elements: 1_000,
        },
    }
}

#[test]
fn mainnet_activation_uses_known_height() {
    assert_eq!(witness_rules(481_823), WitnessRules::BeforeActivation);
    assert_eq!(witness_rules(481_824), WitnessRules::Active);
    assert_eq!(witness_rules(u64::MAX), WitnessRules::Active);
}

// This tests the shared block exchange, not mainnet contextual admission of a
// testnet header. The public service only requests its retained mainnet headers.
#[tokio::test]
async fn real_witness_transport_and_stripped_witness_rejection() {
    let original: Block = deserialize(include_bytes!("../tests/data/testnet-segwit.raw")).unwrap();
    for strip in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut block = original.clone();
        if strip {
            for tx in &mut block.txdata {
                for input in &mut tx.input {
                    input.witness = Witness::new();
                }
            }
        }
        let hash = block.block_hash();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 61]; // header + CompactSize(1) + inventory entry.
            stream.read_exact(&mut request).await.unwrap();
            let request: RawNetworkMessage = deserialize(&request).unwrap();
            assert_eq!(
                request.into_payload(),
                NetworkMessage::GetData(vec![Inventory::WitnessBlock(hash)])
            );
            stream
                .write_all(&serialize(&RawNetworkMessage::new(
                    Network::Bitcoin.magic(),
                    NetworkMessage::Block(block),
                )))
                .await
                .unwrap();
        });
        let mut stream = PeerConnection {
            stream: TcpStream::connect(addr).await.unwrap(),
            addr,
            services: ServiceFlags::NETWORK | ServiceFlags::WITNESS,
            relay: true,
            learned: Vec::new(),
            requested_addresses: false,
            idle_window: tokio::time::Instant::now(),
            idle_bytes: 256 * 1024,
            idle_frames: 64,
            observer: None,
        };
        let result = request_block(
            &mut stream,
            hash,
            WitnessRules::Active,
            &mut 10_000,
            config(),
        )
        .await;
        if strip {
            assert!(matches!(result, Err(BlockPeerError::Body(_))));
        } else {
            assert_eq!(result.unwrap(), original);
        }
        server.await.unwrap();
    }
}
use crate::peer_connection::PeerConnection;
use bitcoin::p2p::ServiceFlags;

/// Synthetic, mechanically admissible bodies, never mined or script-validated.
/// Exercise the shared exchange below HeaderSync's mandatory mainnet admission.
fn adversarial_witness_body(many_items: bool) -> Block {
    use bitcoin::blockdata::constants::genesis_block;
    use bitcoin::hashes::Hash;
    use bitcoin::{Amount, OutPoint, ScriptBuf, TxOut, Txid};

    let mut block = genesis_block(Network::Bitcoin);
    let mut transaction = block.txdata[0].clone();
    transaction.input[0].previous_output = OutPoint::new(Txid::from_byte_array([1; 32]), 0);
    // Two transactions, two inputs, three outputs, one coinbase witness item,
    // and 999,992 other witness items reach exactly 1,000,000 elements.
    let empty_items = if many_items { 999_991 } else { 0 };
    let blob = vec![0; 3_990_000 - empty_items];
    let mut items = vec![&[][..]; empty_items];
    items.push(&blob);
    transaction.input[0].witness = Witness::from_slice(&items);
    block.txdata.push(transaction);
    block.txdata[0].input[0].witness = Witness::from_slice(&[[0; 32]]);
    let commitment = Block::compute_witness_commitment(&block.witness_root().unwrap(), &[0; 32]);
    let mut script = vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
    script.extend_from_slice(commitment.as_byte_array());
    block.txdata[0].output.push(TxOut {
        value: Amount::ZERO,
        script_pubkey: ScriptBuf::from_bytes(script),
    });
    block.header.merkle_root = block.compute_merkle_root().unwrap();
    block
}

/// Send only an envelope for early-rejection cases: success cannot depend on
/// consuming a body the peer never supplied. Every exchange owns a fresh socket.
async fn adversarial_exchange(
    wire: &[u8],
    hash: bitcoin::BlockHash,
    limits: BlockDecodeLimits,
    budget: usize,
    envelope_only: bool,
) -> (Result<Block, BlockPeerError>, usize) {
    use std::time::Duration;

    tokio::time::timeout(Duration::from_secs(15), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let mut connection = PeerConnection {
            stream: client,
            addr,
            services: ServiceFlags::NETWORK | ServiceFlags::WITNESS,
            relay: true,
            learned: Vec::new(),
            requested_addresses: false,
            idle_window: tokio::time::Instant::now(),
            idle_bytes: 256 * 1024,
            idle_frames: 64,
            observer: None,
        };
        let mut remaining = budget;
        let client = async {
            let result = request_block(
                &mut connection,
                hash,
                WitnessRules::Active,
                &mut remaining,
                BlockDownloadConfig {
                    decode: limits,
                    ..config()
                },
            )
            .await;
            // Match the enclosing download's failure ownership: never reuse a
            // rejected/partial frame. A new exchange is not pool retry evidence.
            drop(connection);
            (result, remaining)
        };
        let server = async {
            let mut request = [0; 61];
            server.read_exact(&mut request).await.unwrap();
            assert_eq!(
                deserialize::<RawNetworkMessage>(&request)
                    .unwrap()
                    .into_payload(),
                NetworkMessage::GetData(vec![Inventory::WitnessBlock(hash)])
            );
            server
                .write_all(if envelope_only { &wire[..24] } else { wire })
                .await
                .unwrap();
            // Keep the writer open so early rejection is not merely an EOF.
            let mut byte = [0];
            assert_eq!(server.read(&mut byte).await.unwrap(), 0);
        };
        let (result, ()) = tokio::join!(client, server);
        result
    })
    .await
    .expect("bounded adversarial exchange")
}

#[tokio::test]
async fn adversarial_witness_transport_boundaries_and_fresh_exchange_recovery() {
    use crate::decode_error::DecodeError;
    use crate::frame_error::FrameError;
    use crate::sync_error::SyncError;

    for many_items in [false, true] {
        let block = adversarial_witness_body(many_items);
        let hash = block.block_hash();
        let payload = serialize(&block);
        let elements = block.txdata.len()
            + block
                .txdata
                .iter()
                .map(|tx| {
                    tx.input.len()
                        + tx.output.len()
                        + tx.input
                            .iter()
                            .map(|input| input.witness.len())
                            .sum::<usize>()
                })
                .sum::<usize>();
        assert_eq!(elements, if many_items { 1_000_000 } else { 9 });
        assert!((3_990_000..4_000_000).contains(&payload.len()));
        assert!(block.weight().to_wu() <= 4_000_000);
        println!(
            "many_items={many_items} elements={elements} payload_bytes={} weight={}",
            payload.len(),
            block.weight().to_wu()
        );
        let wire = serialize(&RawNetworkMessage::new(
            Network::Bitcoin.magic(),
            NetworkMessage::Block(block),
        ));
        let limits = BlockDecodeLimits {
            max_payload_bytes: payload.len(),
            max_elements: elements,
        };
        for boundary in ["elements", "payload", "exchange", "checksum"] {
            let mut rejected_limits = limits;
            let mut budget = wire.len();
            let mut rejected_wire = wire.clone();
            match boundary {
                "elements" => rejected_limits.max_elements -= 1,
                "payload" => rejected_limits.max_payload_bytes -= 1,
                "exchange" => budget -= 1,
                "checksum" => rejected_wire[20] ^= 1,
                _ => unreachable!(),
            }
            let early = matches!(boundary, "payload" | "exchange");
            let (result, remaining) =
                adversarial_exchange(&rejected_wire, hash, rejected_limits, budget, early).await;
            match boundary {
                "elements" => assert!(matches!(
                    result,
                    Err(BlockPeerError::Body(FrameError::Block(
                        DecodeError::ElementLimit
                    )))
                )),
                "payload" => assert!(matches!(
                    result,
                    Err(BlockPeerError::Peer(SyncError::Protocol(
                        "block frame admission"
                    )))
                )),
                "exchange" => assert!(matches!(
                    result,
                    Err(BlockPeerError::Peer(SyncError::Protocol(
                        "frame or exchange byte budget"
                    )))
                )),
                "checksum" => assert!(matches!(
                    result,
                    Err(BlockPeerError::Peer(SyncError::Protocol("checksum")))
                )),
                _ => unreachable!(),
            }
            assert_eq!(remaining, if early { budget - 24 } else { 0 });
            drop(rejected_wire);
            let (recovered, remaining) =
                adversarial_exchange(&wire, hash, limits, wire.len(), false).await;
            assert_eq!(remaining, 0);
            // Byte-for-byte witness roundtrip, not just a header/txid comparison.
            assert_eq!(serialize(&recovered.unwrap()), payload);
        }
    }
}
