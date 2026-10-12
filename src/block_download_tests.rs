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
        let mut stream = TcpStream::connect(addr).await.unwrap();
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
