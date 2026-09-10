use std::time::Duration;

use gpui::{TestAppContext, frame_stats};
use nmt_profiling::transcript::{Operation, Probe, take_samples};

use crate::profiling::initialize;

struct Enabled;

impl Drop for Enabled {
    fn drop(&mut self) {
        nmt_profiling::set_enabled(false);
        take_samples();
    }
}

#[gpui::test]
fn reports_only_after_enabled_startup_and_drains_final_samples(cx: &mut TestAppContext) {
    let _enabled = Enabled;
    nmt_profiling::set_enabled(false);
    cx.update(initialize);
    nmt_profiling::set_enabled(true);
    assert!(frame_stats::enabled());
    drop(Probe::start(Operation::AppendEntry));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(take_samples()[Operation::AppendEntry as usize].calls, 1);

    cx.update(initialize);
    cx.run_until_parked();
    drop(Probe::start(Operation::AppendDelta));
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(take_samples().iter().all(|total| total.calls == 0));

    drop(Probe::start(Operation::MergeCompleted));
    cx.quit();
    assert!(take_samples().iter().all(|total| total.calls == 0));
}
