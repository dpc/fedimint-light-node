use bitcoin::consensus::{deserialize, serialize};

use super::output;

#[test]
fn publish_canonical_witness_bytes_and_never_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("block");
    let block = deserialize(include_bytes!("../../../tests/data/testnet-segwit.raw")).unwrap();
    output::block(&path, &block).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), serialize(&block));
    assert!(output::block(&path, &block).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), serialize(&block));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
