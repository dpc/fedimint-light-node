use bitcoin::blockdata::constants::genesis_block;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::Hash;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::{Block, BlockHash, Network};
use fedimint_light_node::block_decode::BlockDecodeLimits;
use fedimint_light_node::block_integrity::WitnessRules;
use fedimint_light_node::decode_error::DecodeError;
use fedimint_light_node::frame_error::FrameError;
use fedimint_light_node::integrity_error::IntegrityError;
use fedimint_light_node::v1_block_frame::V1BlockFrame;

fn limits(bytes: usize) -> BlockDecodeLimits {
    BlockDecodeLimits {
        max_payload_bytes: bytes,
        max_elements: 100_000,
    }
}

fn wire(block: Block, network: Network) -> Vec<u8> {
    serialize(&RawNetworkMessage::new(
        network.magic(),
        NetworkMessage::Block(block),
    ))
}

fn parse(header: &[u8]) -> Result<V1BlockFrame, FrameError> {
    V1BlockFrame::parse(header, Network::Bitcoin, limits(usize::MAX))
}

#[test]
fn dependency_frames_reach_integrity_with_explicit_context() {
    for (block, network) in [
        (genesis_block(Network::Bitcoin), Network::Bitcoin),
        (
            deserialize(include_bytes!("data/testnet-segwit.raw")).unwrap(),
            Network::Testnet,
        ),
    ] {
        let bytes = wire(block.clone(), network);
        let admitted =
            V1BlockFrame::parse(&bytes[..24], network, limits(bytes.len() - 24)).unwrap();
        assert_eq!(admitted.payload_len(), bytes.len() - 24);
        assert_eq!(
            admitted
                .decode(&bytes[24..], block.block_hash(), WitnessRules::Active)
                .unwrap(),
            block
        );
        assert!(matches!(
            admitted.decode(&bytes[24..], BlockHash::all_zeros(), WitnessRules::Active),
            Err(FrameError::Block(DecodeError::Integrity(_)))
        ));
        let admitted = V1BlockFrame::parse(
            &bytes[..24],
            network,
            BlockDecodeLimits {
                max_payload_bytes: bytes.len() - 24,
                max_elements: 0,
            },
        )
        .unwrap();
        assert!(matches!(
            admitted.decode(&bytes[24..], block.block_hash(), WitnessRules::Active),
            Err(FrameError::Block(DecodeError::ElementLimit))
        ));
    }
}

#[test]
fn fixed_header_encoding_has_known_magic_length_and_checksum() {
    // Bitcoin genesis serialized payload is 285 bytes; SHA256d starts f7 1a 24 03.
    let bytes = wire(genesis_block(Network::Bitcoin), Network::Bitcoin);
    assert_eq!(
        &bytes[..24],
        &[
            0xf9, 0xbe, 0xb4, 0xd9, b'b', b'l', b'o', b'c', b'k', 0, 0, 0, 0, 0, 0, 0, 0x1d, 0x01,
            0, 0, 0xf7, 0x1a, 0x24, 0x03,
        ]
    );
}

#[test]
fn header_rejects_wrong_network_and_every_noncanonical_command_byte() {
    let bytes = wire(genesis_block(Network::Bitcoin), Network::Bitcoin);
    for network in [
        Network::Testnet,
        Network::Testnet4,
        Network::Signet,
        Network::Regtest,
    ] {
        assert!(matches!(
            V1BlockFrame::parse(&bytes[..24], network, limits(4_000_000)),
            Err(FrameError::Magic)
        ));
    }
    for index in 4..16 {
        for value in 0..=255 {
            if value == bytes[index] {
                continue;
            }
            let mut header = bytes[..24].to_vec();
            header[index] = value;
            assert!(matches!(parse(&header), Err(FrameError::Command)));
        }
    }
    let ping = serialize(&RawNetworkMessage::new(
        Network::Bitcoin.magic(),
        NetworkMessage::Ping(1),
    ));
    assert!(matches!(parse(&ping[..24]), Err(FrameError::Command)));
}

#[test]
fn declared_sizes_are_bounded_before_body_exists_without_addition_overflow() {
    let mut header = wire(genesis_block(Network::Bitcoin), Network::Bitcoin)[..24].to_vec();
    for length in [0_u32, 1, 4_000_000] {
        header[16..20].copy_from_slice(&length.to_le_bytes());
        assert_eq!(parse(&header).unwrap().payload_len(), length as usize);
    }
    for length in [4_000_001_u32, u32::MAX] {
        header[16..20].copy_from_slice(&length.to_le_bytes());
        assert!(matches!(parse(&header), Err(FrameError::PayloadLimit)));
    }
    header[16..20].copy_from_slice(&285_u32.to_le_bytes());
    assert!(V1BlockFrame::parse(&header, Network::Bitcoin, limits(285)).is_ok());
    assert!(matches!(
        V1BlockFrame::parse(&header, Network::Bitcoin, limits(284)),
        Err(FrameError::PayloadLimit)
    ));
    assert!(matches!(
        V1BlockFrame::parse(&header, Network::Bitcoin, limits(0)),
        Err(FrameError::PayloadLimit)
    ));
}

#[test]
fn every_truncation_trailing_bytes_and_corruption_are_rejected() {
    let block = genesis_block(Network::Bitcoin);
    let bytes = wire(block.clone(), Network::Bitcoin);
    for end in 0..24 {
        assert!(matches!(
            parse(&bytes[..end]),
            Err(FrameError::HeaderLength)
        ));
    }
    assert!(matches!(parse(&bytes[..25]), Err(FrameError::HeaderLength)));
    let admitted = parse(&bytes[..24]).unwrap();
    for end in 24..bytes.len() {
        assert!(matches!(
            admitted.decode(&bytes[24..end], block.block_hash(), WitnessRules::Active),
            Err(FrameError::PayloadLength)
        ));
    }
    let mut body = bytes[24..].to_vec();
    body.push(0);
    assert!(matches!(
        admitted.decode(&body, block.block_hash(), WitnessRules::Active),
        Err(FrameError::PayloadLength)
    ));
    body.pop();
    body[0] ^= 1;
    assert!(matches!(
        admitted.decode(&body, block.block_hash(), WitnessRules::Active),
        Err(FrameError::Checksum)
    ));
    let mut bad_header = bytes[..24].to_vec();
    bad_header[20] ^= 1;
    assert!(matches!(
        parse(&bad_header)
            .unwrap()
            .decode(&bytes[24..], block.block_hash(), WitnessRules::Active),
        Err(FrameError::Checksum)
    ));
}

#[test]
fn caller_can_collect_arbitrary_chunks_after_fixed_header_admission() {
    let block = genesis_block(Network::Bitcoin);
    let bytes = wire(block.clone(), Network::Bitcoin);
    for chunk_size in 1..=bytes.len() {
        let mut header = [0; 24];
        let mut filled = 0;
        let mut admitted = None;
        let mut body = Vec::new();
        for mut chunk in bytes.chunks(chunk_size) {
            if filled < 24 {
                let take = chunk.len().min(24 - filled);
                header[filled..filled + take].copy_from_slice(&chunk[..take]);
                filled += take;
                chunk = &chunk[take..];
                if filled == 24 {
                    let frame = parse(&header).unwrap();
                    body.reserve_exact(frame.payload_len());
                    admitted = Some(frame);
                }
            }
            body.extend_from_slice(chunk);
        }
        assert_eq!(
            admitted
                .unwrap()
                .decode(&body, block.block_hash(), WitnessRules::Active)
                .unwrap(),
            block
        );
    }
}

#[test]
fn checksum_success_does_not_establish_body_integrity() {
    let mut block = genesis_block(Network::Bitcoin);
    block.header.merkle_root = bitcoin::TxMerkleNode::all_zeros();
    let bytes = wire(block.clone(), Network::Bitcoin);
    assert!(matches!(
        parse(&bytes[..24])
            .unwrap()
            .decode(&bytes[24..], block.block_hash(), WitnessRules::Active),
        Err(FrameError::Block(DecodeError::Integrity(
            IntegrityError::MerkleRoot
        )))
    ));
}
