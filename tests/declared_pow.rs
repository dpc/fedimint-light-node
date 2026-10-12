use bitcoin::blockdata::constants::genesis_block;
use bitcoin::{CompactTarget, Network, Target};
use fedimint_light_node::declared_pow::check_declared_pow;
use fedimint_light_node::integrity_error::IntegrityError;

#[test]
fn real_genesis_pow_passes_but_changed_nonce_does_not() {
    let mut header = genesis_block(Network::Bitcoin).header;
    let limit = header.target();
    assert_eq!(check_declared_pow(&header, limit), Ok(()));
    header.nonce += 1;
    assert_eq!(
        check_declared_pow(&header, limit),
        Err(IntegrityError::DeclaredProofOfWork)
    );
}

#[test]
fn zero_negative_overflow_and_above_limit_targets_fail() {
    let mut header = genesis_block(Network::Bitcoin).header;
    for bits in [
        0, 0x01000001, 0x1d80ffff, 0x23000001, 0x22000100, 0x21010000,
    ] {
        header.bits = CompactTarget::from_consensus(bits);
        assert_eq!(
            check_declared_pow(&header, Target::from_be_bytes([0xff; 32])),
            Err(IntegrityError::DeclaredTarget),
            "{bits:08x}"
        );
    }
    header = genesis_block(Network::Regtest).header;
    assert_eq!(
        check_declared_pow(&header, Target::MAX_ATTAINABLE_REGTEST),
        Ok(())
    );
    assert_eq!(
        check_declared_pow(&header, genesis_block(Network::Bitcoin).header.target()),
        Err(IntegrityError::DeclaredTarget)
    );
    assert_eq!(
        check_declared_pow(&header, Target::ZERO),
        Err(IntegrityError::DeclaredTarget)
    );
}

#[test]
fn positive_noncanonical_encoding_is_not_an_expected_difficulty_check() {
    let mut header = genesis_block(Network::Regtest).header;
    // 0x21007f00 and 0x207f0000 encode the same positive target.
    header.bits = CompactTarget::from_consensus(0x21007f00);
    assert_ne!(header.bits, header.target().to_compact_lossy());
    while !header.target().is_met_by(header.block_hash()) {
        header.nonce += 1;
    }
    assert_eq!(
        check_declared_pow(&header, Target::MAX_ATTAINABLE_REGTEST),
        Ok(())
    );
}

#[test]
fn compact_overflow_boundaries_accept_largest_nonoverflowing_words() {
    let mut header = genesis_block(Network::Regtest).header;
    for bits in [0x220000ff, 0x2100ffff, 0x207fffff] {
        header.bits = CompactTarget::from_consensus(bits);
        while !header.target().is_met_by(header.block_hash()) {
            header.nonce += 1;
        }
        assert_eq!(
            check_declared_pow(&header, Target::from_be_bytes([0xff; 32])),
            Ok(()),
            "{bits:08x}"
        );
    }
}
