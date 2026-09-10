use std::hint::black_box;

use crate::allocation::AllocationCounts;
use crate::transcript::{Operation, Probe, take_samples};

#[test]
fn switch_controls_inclusive_thread_local_samples_and_draining() {
    let _lock = crate::TEST_LOCK.lock();
    crate::set_enabled(false);
    assert!(Probe::start(Operation::Replay).is_none());
    crate::set_enabled(true);
    let outer = Probe::start(Operation::Replay).unwrap();
    let inner = Probe::start(Operation::AppendEntry).unwrap();
    let buffer = black_box(vec![0_u8; 512]);
    black_box(&buffer);
    drop(buffer);
    drop(inner);
    drop(outer);

    let totals = take_samples();
    let replay = totals[Operation::Replay as usize];
    let append = totals[Operation::AppendEntry as usize];
    assert_eq!(replay.calls, 1);
    assert_eq!(append.calls, 1);
    assert_eq!(replay.allocation_samples, 1);
    assert_eq!(append.allocations.allocations, 1);
    assert_eq!(append.allocations.allocated_bytes, 512);
    assert_eq!(append.allocations.deallocations, 1);
    assert_eq!(replay.allocations, append.allocations);
    assert!(replay.elapsed >= append.elapsed);
    assert!(take_samples().iter().all(|total| total.calls == 0));
    crate::set_enabled(false);
    assert!(Probe::start(Operation::AppendDelta).is_none());
    assert_eq!(
        take_samples()[Operation::AppendDelta as usize].allocations,
        AllocationCounts::default()
    );
}
