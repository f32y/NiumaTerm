use std::alloc::{GlobalAlloc as _, Layout};
use std::hint::black_box;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Barrier};
use std::thread;

use crate::allocation::{AllocationCounts, AllocationScope, ProfilingAllocator};

#[global_allocator]
static ALLOCATOR: ProfilingAllocator = ProfilingAllocator;

#[test]
fn counts_scoped_calls_nested_work_and_unwind_without_other_threads() {
    let _lock = crate::TEST_LOCK.lock();

    ALLOCATOR.set_enabled(false);

    assert!(AllocationScope::start().is_none());

    ALLOCATOR.set_enabled(true);

    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = barrier.clone();

    let worker = thread::spawn(move || {
        worker_barrier.wait();

        let buffer = black_box(vec![0_u8; 512]);

        black_box(&buffer);
        drop(buffer);
        worker_barrier.wait();
    });

    let small = Layout::from_size_align(8, 32).unwrap();
    let large = Layout::from_size_align(64, 32).unwrap();
    let zeroed = Layout::from_size_align(16, 16).unwrap();
    let outer = AllocationScope::start().unwrap();

    barrier.wait();
    barrier.wait();

    // SAFETY: These layouts are nonzero and valid. Both pointers are checked
    // before use, and deallocation uses the layout of each live allocation.
    let pointer = unsafe { ALLOCATOR.alloc(small) };

    assert!(!pointer.is_null());
    assert_eq!(pointer.addr() % 32, 0);

    let inner = AllocationScope::start().unwrap();

    // SAFETY: The pointer belongs to this allocator with layout `small`.
    let pointer = unsafe { ALLOCATOR.realloc(pointer, small, large.size()) };

    assert!(!pointer.is_null());

    // SAFETY: `zeroed` is a valid nonzero allocation layout.
    let zeros = unsafe { ALLOCATOR.alloc_zeroed(zeroed) };

    assert!(!zeros.is_null());
    // SAFETY: The allocation contains at least one initialized byte.
    assert_eq!(unsafe { zeros.read() }, 0);

    let inner_counts = inner.finish();

    // SAFETY: Both pointers are live, and these are their current layouts.
    unsafe {
        ALLOCATOR.dealloc(pointer, large);
        ALLOCATOR.dealloc(zeros, zeroed);
    }

    let outer_counts = outer.finish();

    worker.join().unwrap();

    assert_eq!(inner_counts.allocations, 1);
    assert_eq!(inner_counts.allocated_bytes, 16);
    assert_eq!(inner_counts.reallocations, 1);
    assert_eq!(inner_counts.reallocated_bytes, 64);
    assert_eq!(inner_counts.deallocations, 0);
    assert_eq!(
        outer_counts,
        AllocationCounts {
            allocations: 2,
            reallocations: 1,
            deallocations: 2,
            allocated_bytes: 24,
            reallocated_bytes: 64,
            deallocated_bytes: 80,
        }
    );

    let result = catch_unwind(AssertUnwindSafe(|| {
        let _scope = AllocationScope::start().unwrap();

        panic!("exercise scope cleanup");
    }));

    assert!(result.is_err());
    assert_eq!(
        AllocationScope::start().unwrap().finish(),
        AllocationCounts::default()
    );

    ALLOCATOR.set_enabled(false);

    assert!(AllocationScope::start().is_none());
}
