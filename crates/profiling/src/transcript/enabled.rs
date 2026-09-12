//! Transcript operation costs controlled by the shared profiling switch.
//! Samples are inclusive of nested operations on the same thread; do not add
//! replay and append totals together. Collection does not retain message text.

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::cell::RefCell;
use std::mem;
use std::time::{Duration, Instant};

use tracing::info;

use crate::allocation::{AllocationCounts, AllocationScope};
use crate::transcript::Operation;

/// Operations in the same order as the drained totals array.
pub const OPERATIONS: [Operation; 9] = [
    Operation::AppendEntry,
    Operation::Replay,
    Operation::MergeCompleted,
    Operation::AppendDelta,
    Operation::RowsRebuild,
    Operation::Typewriter,
    Operation::MirrorRebuild,
    Operation::BackgroundSnapshot,
    Operation::WorkflowSnapshot,
];

/// Inclusive measurements accumulated for one operation on the calling thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub calls: u64,
    pub elapsed: Duration,
    pub max_elapsed: Duration,
    pub allocation_samples: u64,
    pub allocations: AllocationCounts,
}

thread_local! {
    // All transcript operations run synchronously on the UI thread. Reporting
    // on that same thread avoids a shared lock in each measured operation.
    static TOTALS: RefCell<[Totals; OPERATIONS.len()]> = RefCell::new([Totals::default(); OPERATIONS.len()]);
}

/// A synchronous operation's elapsed time and Rust allocation traffic.
/// This guard is deliberately thread-bound and must not cross an await.
#[must_use]
pub struct Probe {
    operation: Operation,
    started: Instant,
    allocations: Option<AllocationScope>,
}

impl Probe {
    /// Skip clock reads and allocation scopes while profiling is disabled.
    pub fn start(operation: Operation) -> Option<Self> {
        if !crate::enabled() {
            return None;
        }

        // Initialize the accumulator before timing or counting the first sample.
        TOTALS.with(|_| {});

        Some(Self {
            operation,
            allocations: AllocationScope::start(),
            started: Instant::now(),
        })
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        let allocations = self.allocations.take().map(AllocationScope::finish);

        TOTALS.with_borrow_mut(|totals| {
            let total = &mut totals[self.operation as usize];

            total.calls += 1;
            total.elapsed += elapsed;
            total.max_elapsed = total.max_elapsed.max(elapsed);

            if let Some(allocations) = allocations {
                total.allocation_samples += 1;
                total.allocations.accumulate(allocations);
            }
        });
    }
}

/// Drain the calling UI thread's samples to the normal application log.
/// Call at shutdown too, so a final partial reporting interval is not lost.
pub fn flush() {
    let totals = take_samples();

    for (operation, total) in OPERATIONS.into_iter().zip(totals) {
        if total.calls == 0 {
            continue;
        }

        let operation: &str = operation.into();

        info!(
            target: "transcript_perf",
            operation,
            calls = total.calls,
            total_us = total.elapsed.as_secs_f64() * 1_000_000.0,
            avg_us = total.elapsed.as_secs_f64() * 1_000_000.0 / total.calls as f64,
            max_us = total.max_elapsed.as_secs_f64() * 1_000_000.0,
            allocation_samples = total.allocation_samples,
            alloc = total.allocations.allocations,
            realloc = total.allocations.reallocations,
            dealloc = total.allocations.deallocations,
            allocated_bytes = total.allocations.allocated_bytes,
            reallocated_bytes = total.allocations.reallocated_bytes,
            deallocated_bytes = total.allocations.deallocated_bytes,
            "transcript operation costs (inclusive, current thread)"
        );
    }
}

/// Drain this thread's operation totals without copying transcript content.
pub fn take_samples() -> [Totals; OPERATIONS.len()] {
    TOTALS.with_borrow_mut(mem::take)
}
