use bitcoin::hashes::{Hash, sha256d};

use super::merkle_root;
use crate::integrity_error::IntegrityError;

#[test]
fn merkle_mutation_checks_internal_levels_before_odd_padding() {
    let a = sha256d::Hash::hash(b"a");
    let b = sha256d::Hash::hash(b"b");
    let c = sha256d::Hash::hash(b"c");
    assert!(merkle_root(&mut vec![a, b, c]).is_ok());
    assert_eq!(
        merkle_root(&mut vec![a, b, c, c]),
        Err(IntegrityError::MutatedMerkleTree)
    );
    assert_eq!(
        merkle_root(&mut vec![a, b, a, b]),
        Err(IntegrityError::MutatedMerkleTree)
    );
}
