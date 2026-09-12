use std::time::Duration;

use crate::frame_stats::enabled::STATS;
use crate::frame_stats::{
    enabled, record_draw, record_main_thread_task, record_redraws_requested, set_enabled,
    start_timer,
};

#[test]
fn enabled_collection_records_samples_and_disabling_clears_them() {
    let _lock = crate::TEST_LOCK.lock();

    set_enabled(true);

    assert!(enabled());
    assert!(start_timer().is_some());

    record_draw(Duration::from_millis(2), 3);
    record_main_thread_task(Duration::from_millis(5));
    record_redraws_requested(2);

    {
        let stats = STATS.lock();

        assert_eq!(stats.draw.count, 1);
        assert_eq!(stats.draw.total_us, 2_000);
        assert_eq!(stats.dirty_views, 3);
        assert_eq!(stats.main_tasks.total_us, 5_000);
        assert_eq!(stats.redraws_requested, 2);
    }

    set_enabled(false);

    assert!(start_timer().is_none());

    record_draw(Duration::from_millis(10), 9);

    let stats = STATS.lock();

    assert_eq!(stats.draw.count, 0);
    assert_eq!(stats.dirty_views, 0);
}
