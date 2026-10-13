//! Seed both coverage-guided targets from existing public fixtures.

use bitcoin::consensus::{deserialize, serialize};
use bitcoin::{Block, Network};

fn main() {
    let root = std::env::args().nth(1).expect("corpus output directory");
    let root = std::path::Path::new(&root);
    for name in ["block", "transaction"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    for (name, block) in [
        (
            "genesis",
            bitcoin::blockdata::constants::genesis_block(Network::Bitcoin),
        ),
        (
            "segwit",
            deserialize::<Block>(include_bytes!("../../tests/data/testnet-segwit.raw")).unwrap(),
        ),
    ] {
        std::fs::write(root.join("block").join(name), serialize(&block)).unwrap();
        for (index, transaction) in block.txdata.iter().enumerate() {
            std::fs::write(
                root.join("transaction").join(format!("{name}-{index}")),
                serialize(transaction),
            )
            .unwrap();
        }
    }
}
