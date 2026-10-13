use super::*;

fn headers() -> Vec<Header> {
    include_bytes!("../tests/data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .map(|bytes| bitcoin::consensus::deserialize(bytes).unwrap())
        .collect()
}

fn anchor(headers: &[Header], height: usize) -> StoreAnchor {
    StoreAnchor {
        network: Network::Bitcoin,
        height: height as u64,
        hash: headers[height].block_hash(),
        height_one_hash: headers[1].block_hash(),
    }
}

#[test]
fn historical_context_is_exact_bounded_and_hash_linked() {
    let headers = headers();
    for height in [0, 1, 10, 11, 2015, 2016, 2026, 2035] {
        let first = HeaderContext::context_start(height as u64) as usize;
        let history = &headers[first..=height];
        let context =
            HeaderContext::from_trusted_history(anchor(&headers, height), history).unwrap();
        assert_eq!(context.relative_work(), Work::from_be_bytes([0; 32]));
        assert!(history.len() <= 2016);
        assert!(matches!(
            HeaderContext::from_trusted_history(anchor(&headers, height), &history[1..]),
            Err(HeaderError::MissingContext)
        ));
        let mut broken = history.to_vec();
        broken[0].nonce ^= 1;
        assert!(matches!(
            HeaderContext::from_trusted_history(anchor(&headers, height), &broken),
            Err(HeaderError::Linkage)
        ));
    }
    for network in [
        Network::Testnet,
        Network::Testnet4,
        Network::Signet,
        Network::Regtest,
    ] {
        let mut anchor = anchor(&headers, 0);
        anchor.network = network;
        assert!(matches!(
            HeaderContext::from_trusted_history(anchor, &headers[..1]),
            Err(HeaderError::UnsupportedNetwork)
        ));
    }
}

#[test]
fn real_mainnet_headers_cross_retarget_and_preserve_relative_work() {
    let headers = headers();
    let mut context =
        HeaderContext::from_trusted_history(anchor(&headers, 0), &headers[..1]).unwrap();
    let mut work = Work::from_be_bytes([0; 32]);
    for header in &headers[1..] {
        assert_eq!(context.expected_target().unwrap(), header.bits);
        context = context
            .validate_next(*header, u64::from(header.time))
            .unwrap();
        work = checked_work(work, header.work()).unwrap();
        assert_eq!(context.relative_work(), work);
        let decoded = HeaderContext::decode(&context.encode(), context.anchor).unwrap();
        assert_eq!(decoded.encode(), context.encode());
    }
    assert_eq!(context.period_start, headers[2016].time);
    assert_eq!(
        context.times,
        headers[2025..2036]
            .iter()
            .map(|h| h.time)
            .collect::<Vec<_>>()[..]
    );
}

#[test]
fn retarget_clamps_signed_intervals_and_compact_rounding() {
    let headers = headers();
    let mut context =
        HeaderContext::from_trusted_history(anchor(&headers, 2015), &headers[..2016]).unwrap();
    context.tip.bits = CompactTarget::from_consensus(0x1c0ffff0);
    context.period_start = 10_000_000;
    for (span, expected) in [
        (-1_i64, 0x1c03fffc),
        (0, 0x1c03fffc),
        (302_399, 0x1c03fffc),
        (302_400, 0x1c03fffc),
        (1_209_600, 0x1c0ffff0),
        (4_838_400, 0x1c3fffc0),
        (4_838_401, 0x1c3fffc0),
    ] {
        context.tip.time = (i64::from(context.period_start) + span) as u32;
        assert_eq!(context.expected_target().unwrap().to_consensus(), expected);
    }
    context.tip.bits = CompactTarget::from_consensus(0x1d00ffff);
    assert_eq!(
        context.expected_target().unwrap().to_consensus(),
        0x1d00ffff
    );
    // Bitcoin Core pow_tests' independent mainnet retarget vector.
    context.period_start = 1_261_130_161;
    context.tip.time = 1_262_152_739;
    assert_eq!(
        context.expected_target().unwrap().to_consensus(),
        0x1d00d86a
    );
    // The same timestamps must not retarget at a non-boundary.
    context.height = 2014;
    assert_eq!(context.expected_target().unwrap(), context.tip.bits);
}

#[test]
fn timestamp_boundaries_use_upper_median_and_injected_clock() {
    let headers = headers();
    let context = HeaderContext::from_trusted_history(anchor(&headers, 0), &headers[..1]).unwrap();
    let next = headers[1];
    assert!(
        context
            .validate_next(next, u64::from(next.time) - 7200)
            .is_ok()
    );
    assert!(matches!(
        context.validate_next(next, u64::from(next.time) - 7201),
        Err(HeaderError::FutureTime)
    ));
    let mut context = context.validate_next(next, u64::MAX).unwrap();
    // FutureTime is not proof that the remaining validation rules would pass.
    let mut invalid = headers[2];
    invalid.nonce ^= 1;
    assert!(matches!(
        context.validate_next(invalid, 0),
        Err(HeaderError::FutureTime)
    ));
    assert!(matches!(
        context.validate_next(invalid, u64::from(invalid.time)),
        Err(HeaderError::ProofOfWork(_))
    ));
    // Two timestamps: use the upper median, not mean or lower median.
    context.times[..2].copy_from_slice(&[headers[2].time, 0]);
    assert!(matches!(
        context.validate_next(headers[2], u64::MAX),
        Err(HeaderError::MedianTime)
    ));
    context.times[..2].copy_from_slice(&[headers[2].time - 1, 0]);
    assert!(context.validate_next(headers[2], u64::MAX).is_ok());
    let mut context =
        HeaderContext::from_trusted_history(anchor(&headers, 10), &headers[..11]).unwrap();
    context.times = [
        0,
        0,
        0,
        0,
        0,
        headers[11].time,
        u32::MAX,
        u32::MAX,
        u32::MAX,
        u32::MAX,
        u32::MAX,
    ];
    assert!(matches!(
        context.validate_next(headers[11], u64::MAX),
        Err(HeaderError::MedianTime)
    ));
    context.times[5] -= 1;
    assert!(context.validate_next(headers[11], u64::MAX).is_ok());
}

#[test]
fn declared_pow_does_not_prove_expected_difficulty_or_linkage() {
    let headers = headers();
    let mut context =
        HeaderContext::from_trusted_history(anchor(&headers, 0), &headers[..1]).unwrap();
    check_declared_pow(&headers[1], Target::MAX_ATTAINABLE_MAINNET).unwrap();
    // Keep the real parent hash, but independently alter the expected rule input.
    // A real valid-PoW child must still fail contextual target equality.
    context.height = 2015;
    context.period_start = context.tip.time;
    assert!(matches!(
        context.validate_next(headers[1], u64::MAX),
        Err(HeaderError::Difficulty)
    ));
    context.height = 0;
    assert!(matches!(
        context.validate_next(headers[2], u64::MAX),
        Err(HeaderError::Linkage)
    ));
    let mut child = headers[1];
    child.nonce ^= 1;
    context.anchor.height_one_hash = child.block_hash();
    assert!(matches!(
        context.validate_next(child, u64::MAX),
        Err(HeaderError::ProofOfWork(_))
    ));
    for bits in [0, 0x1d80ffff, 0x23000001, 0x207fffff] {
        child.bits = CompactTarget::from_consensus(bits);
        assert!(matches!(
            context.validate_next(child, u64::MAX),
            Err(HeaderError::Difficulty)
        ));
    }
}

#[test]
fn work_is_origin_scoped_checked_and_not_height_scored() {
    let headers = headers();
    let context = HeaderContext::from_trusted_history(anchor(&headers, 0), &headers[..1]).unwrap();
    let next = context.validate_next(headers[1], u64::MAX).unwrap();
    assert_eq!(next.compare_work(&context).unwrap(), Ordering::Greater);
    assert_eq!(context.compare_work(&context).unwrap(), Ordering::Equal);
    let other = HeaderContext::from_trusted_history(anchor(&headers, 1), &headers[..2]).unwrap();
    assert!(matches!(
        next.compare_work(&other),
        Err(HeaderError::DifferentAnchor)
    ));
    let mut shorter = context.clone();
    shorter.work = next.work + next.work;
    assert_eq!(shorter.compare_work(&next).unwrap(), Ordering::Greater);
    let mut overflow = context.clone();
    overflow.work = Work::from_be_bytes([255; 32]);
    assert!(matches!(
        overflow.validate_next(headers[1], u64::MAX),
        Err(HeaderError::Overflow)
    ));
    overflow.height = u64::MAX;
    assert!(matches!(
        overflow.expected_target(),
        Err(HeaderError::Overflow)
    ));
    assert_eq!(
        checked_work(Work::from_be_bytes([0; 32]), next.work).unwrap(),
        next.work
    );
}

#[test]
fn buried_version_floors_are_height_context_not_peer_claims() {
    let headers = headers();
    let mut context =
        HeaderContext::from_trusted_history(anchor(&headers, 0), &headers[..1]).unwrap();
    for (height, obsolete) in [(227_931, 1), (363_725, 2), (388_381, 3)] {
        context.height = height - 1;
        context.times.fill(context.tip.time);
        let mut child = headers[1];
        child.version = bitcoin::block::Version::from_consensus(obsolete);
        assert!(matches!(
            context.validate_next(child, u64::MAX),
            Err(HeaderError::Version)
        ));
    }
}
