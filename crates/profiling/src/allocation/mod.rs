//! Scoped Rust allocation traffic on the calling thread. Native allocations and
//! work dispatched to other threads are not included. Byte counts describe
//! allocator requests, not resident memory or the size of the live heap.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable counting in the installed profiling allocator.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Successful calls and requested sizes inside a measurement scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationCounts {
    pub allocations: u64,
    pub reallocations: u64,
    pub deallocations: u64,
    /// Sizes of successful alloc/alloc_zeroed calls.
    pub allocated_bytes: u64,
    /// New sizes requested by successful realloc calls, including in-place ones.
    pub reallocated_bytes: u64,
    /// Sizes passed to dealloc; realloc's old size is not included.
    pub deallocated_bytes: u64,
}

impl AllocationCounts {
    /// Combine independent observations without interpreting them as live memory.
    pub fn accumulate(&mut self, other: Self) {
        self.allocations = self.allocations.wrapping_add(other.allocations);
        self.reallocations = self.reallocations.wrapping_add(other.reallocations);
        self.deallocations = self.deallocations.wrapping_add(other.deallocations);
        self.allocated_bytes = self.allocated_bytes.wrapping_add(other.allocated_bytes);
        self.reallocated_bytes = self.reallocated_bytes.wrapping_add(other.reallocated_bytes);
        self.deallocated_bytes = self.deallocated_bytes.wrapping_add(other.deallocated_bytes);
    }

    fn since(self, before: Self) -> Self {
        Self {
            allocations: self.allocations.wrapping_sub(before.allocations),
            reallocations: self.reallocations.wrapping_sub(before.reallocations),
            deallocations: self.deallocations.wrapping_sub(before.deallocations),
            allocated_bytes: self.allocated_bytes.wrapping_sub(before.allocated_bytes),
            reallocated_bytes: self
                .reallocated_bytes
                .wrapping_sub(before.reallocated_bytes),
            deallocated_bytes: self
                .deallocated_bytes
                .wrapping_sub(before.deallocated_bytes),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ThreadCounts {
    depth: usize,
    counts: AllocationCounts,
}

thread_local! {
    // Const initialization and Copy storage keep allocator callbacks free of
    // heap allocation, locking, and destructor registration.
    static COUNTS: Cell<ThreadCounts> = const { Cell::new(ThreadCounts {
        depth: 0,
        counts: AllocationCounts {
            allocations: 0, reallocations: 0, deallocations: 0,
            allocated_bytes: 0, reallocated_bytes: 0, deallocated_bytes: 0,
        },
    }) };
}

/// The system allocator with optional, thread-local allocation accounting.
/// Install this as the executable's global allocator before enabling tracking.
pub struct ProfilingAllocator;

impl ProfilingAllocator {
    /// Enable counting for scopes subsequently opened by this executable.
    /// Disabled callbacks only check this switch before returning.
    pub fn set_enabled(&self, enabled: bool) {
        set_enabled(enabled);
    }
}

fn record(change: AllocationCounts) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let _ = COUNTS.try_with(|slot| {
        let mut state = slot.get();

        if state.depth != 0 {
            state.counts.accumulate(change);
            slot.set(state);
        }
    });
}

// SAFETY: Every allocation operation forwards its pointer, layout, and size
// unchanged to System. Accounting never allocates, locks, or unwinds.
unsafe impl GlobalAlloc for ProfilingAllocator {
    /// # Safety
    /// The layout must have nonzero size and satisfy GlobalAlloc's requirements.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc's caller supplies a valid nonzero layout.
        let pointer = unsafe { System.alloc(layout) };

        if !pointer.is_null() {
            record(AllocationCounts {
                allocations: 1,
                allocated_bytes: layout.size() as u64,
                ..AllocationCounts::default()
            });
        }

        pointer
    }

    /// # Safety
    /// The layout must have nonzero size and satisfy GlobalAlloc's requirements.
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc's caller supplies a valid nonzero layout.
        let pointer = unsafe { System.alloc_zeroed(layout) };

        if !pointer.is_null() {
            record(AllocationCounts {
                allocations: 1,
                allocated_bytes: layout.size() as u64,
                ..AllocationCounts::default()
            });
        }

        pointer
    }

    /// # Safety
    /// The pointer must be live with the supplied layout from this allocator.
    /// The new size must be nonzero and valid for that layout's alignment.
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: The caller supplies a live allocation from this allocator,
        // its original layout, and a valid new size. System owns that allocation.
        let result = unsafe { System.realloc(pointer, layout, new_size) };

        if !result.is_null() {
            record(AllocationCounts {
                reallocations: 1,
                reallocated_bytes: new_size as u64,
                ..AllocationCounts::default()
            });
        }

        result
    }

    /// # Safety
    /// The pointer must be live with the supplied layout from this allocator.
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: The caller supplies a live allocation and its original layout;
        // all allocations made by this wrapper come directly from System.
        unsafe { System.dealloc(pointer, layout) };
        record(AllocationCounts {
            deallocations: 1,
            deallocated_bytes: layout.size() as u64,
            ..AllocationCounts::default()
        });
    }
}

/// Inclusive allocation traffic within a synchronous scope on this thread.
/// Nested scopes are allowed; their counts are also included in their parent.
/// Do not hold a scope across an await, since unrelated tasks can share a thread.
#[must_use]
pub struct AllocationScope {
    before: AllocationCounts,
    // A scope must be closed on the thread whose counters it opened.
    thread: PhantomData<Rc<()>>,
}

impl AllocationScope {
    /// Returns None when the installed profiling allocator is disabled.
    pub fn start() -> Option<Self> {
        if !ENABLED.load(Ordering::Relaxed) {
            return None;
        }

        let before = COUNTS.with(|slot| {
            let mut state = slot.get();

            state.depth += 1;
            slot.set(state);
            state.counts
        });

        Some(Self {
            before,
            thread: PhantomData,
        })
    }

    /// Finish measurement before aggregating or formatting the result.
    pub fn finish(self) -> AllocationCounts {
        COUNTS.with(|slot| slot.get().counts.since(self.before))
    }
}

impl Drop for AllocationScope {
    fn drop(&mut self) {
        COUNTS.with(|slot| {
            let mut state = slot.get();

            state.depth -= 1;
            slot.set(state);
        });
    }
}

#[cfg(test)]
mod tests;
