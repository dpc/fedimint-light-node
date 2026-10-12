use std::num::NonZeroU64;

use fedimint_light_node::retained_range::RetainedRange;

#[test]
fn window_includes_tip_and_rejects_expired_and_future_heights() {
    // REQ-bounded-history: advancing the tip does not wait for lagging consumers.
    let range = RetainedRange::new(100, NonZeroU64::new(10).unwrap());
    assert_eq!(range.first(), 91);
    assert_eq!(range.tip(), 100);
    assert!(!range.contains(90));
    assert!(range.contains(91));
    assert!(range.contains(100));
    assert!(!range.contains(101));
    assert!(!RetainedRange::new(101, NonZeroU64::new(10).unwrap()).contains(91));
}

#[test]
fn young_chain_starts_at_genesis() {
    let range = RetainedRange::new(2, NonZeroU64::new(10).unwrap());
    assert_eq!(range.first(), 0);
    assert!(range.contains(0));
}

#[test]
fn capacity_one_retains_only_tip_even_at_integer_limit() {
    let range = RetainedRange::new(u64::MAX, NonZeroU64::new(1).unwrap());
    assert_eq!(range.first(), u64::MAX);
    assert!(range.contains(u64::MAX));
    assert!(!range.contains(u64::MAX - 1));
}

#[test]
fn maximum_capacity_does_not_overflow() {
    let range = RetainedRange::new(u64::MAX, NonZeroU64::new(u64::MAX).unwrap());
    assert_eq!(range.first(), 1);
    assert!(!range.contains(0));
    assert!(range.contains(u64::MAX));
}
