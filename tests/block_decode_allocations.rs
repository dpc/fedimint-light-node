//! Regression guard: malformed payloads must fail before decoded allocation.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use bitcoin::BlockHash;
use bitcoin::hashes::Hash;
use fedimint_light_node::block_decode::{BlockDecodeLimits, decode_block};
use fedimint_light_node::block_integrity::WitnessRules;

thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

struct CountingAllocator;

fn record() {
    let _ = ALLOCATIONS.try_with(|count| {
        if let Some(value) = count.get() {
            count.set(Some(value + 1));
        }
    });
}

// SAFETY: All requests are forwarded unchanged to the system allocator. The
// thread-local counter neither allocates nor changes allocator ownership.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record();
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn assert_no_decode_allocation(bytes: &[u8], max_elements: usize) {
    ALLOCATIONS.with(|count| count.set(Some(0)));
    let result = decode_block(
        bytes,
        BlockHash::all_zeros(),
        WitnessRules::Active,
        BlockDecodeLimits {
            max_payload_bytes: 4_000_000,
            max_elements,
        },
    );
    let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert!(result.is_err());
    assert_eq!(allocations, 0);
}

#[test]
fn preflight_rejections_allocate_nothing() {
    let fixture = include_bytes!("data/testnet-segwit.raw");
    for end in 0..fixture.len() {
        assert_no_decode_allocation(&fixture[..end], usize::MAX);
    }
    assert_no_decode_allocation(fixture, 0);
    let mut huge_count = vec![0; 80];
    huge_count.extend([0xff; 9]);
    assert_no_decode_allocation(&huge_count, usize::MAX);
    let mut trailing = fixture.to_vec();
    trailing.push(0);
    assert_no_decode_allocation(&trailing, usize::MAX);
}
