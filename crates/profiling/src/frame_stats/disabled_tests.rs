use std::mem::size_of;
use std::time::Duration;

use crate::frame_stats;

#[test]
fn runtime_switches_cannot_activate_uncompiled_collectors() {
    frame_stats::set_enabled(true);
    assert!(!crate::enabled());
    assert!(frame_stats::start_timer().is_none());
    assert_eq!(size_of::<Option<frame_stats::Timer>>(), 0);
    frame_stats::set_vsync_interval(Duration::from_millis(8));
    frame_stats::record_frame_armed();
    frame_stats::record_request_serviced();
    frame_stats::record_main_thread_task(Duration::ZERO);
    frame_stats::record_window_message(Duration::ZERO);
    frame_stats::record_draw(Duration::ZERO, 1);
    frame_stats::record_present(Duration::ZERO, 1);
    frame_stats::record_throttled();
    frame_stats::record_gpu_wait(Duration::ZERO);
    frame_stats::record_vsync_tick(false);
    frame_stats::record_redraws_requested(1);
    assert!(!crate::enabled());
}
