//! Empty frame-statistics hooks for builds without performance collection.

#[cfg(test)]
#[path = "disabled_tests.rs"]
mod tests;

use std::time::Duration;

/// No clock can exist without `enable_profiling`.
pub enum Timer {}

impl Timer {
    /// This path is unreachable because a disabled timer cannot be constructed.
    pub fn elapsed(&self) -> Duration {
        match *self {}
    }
}

/// The uninhabited timer type also removes callers' timed branches.
#[inline(always)]
pub fn start_timer() -> Option<Timer> {
    None
}

/// Turn collection and reporting on or off. Counters reset on every change, so
/// a reporting period never mixes samples from either side of the switch and
/// the first interval after enabling is not measured against a stale frame.
#[inline(always)]
pub fn set_enabled(_enabled: bool) {}

/// Whether collection is on. Hot paths that would need a clock read to report
/// an event check this before taking one.
#[inline(always)]
pub fn enabled() -> bool {
    false
}

/// Publish the display refresh interval so the digest can report the frame
/// rate the machine is actually capable of, and classify long frames.
#[inline(always)]
pub fn set_vsync_interval(_interval: Duration) {}

/// Records a frame request arming, so the next draw can report how long the
/// request waited for the UI thread. Only the oldest unserviced request is
/// kept: a later arm before the draw is coalesced into the same frame.
#[inline(always)]
pub fn record_frame_armed() {}

/// Closes out the latency of the frame request the UI thread is now servicing.
/// Called for every serviced request, including ones that present without
/// drawing: a stale arm timestamp left behind by those would charge its whole
/// age to whichever later frame happened to draw.
#[inline(always)]
pub fn record_request_serviced() {}

/// Records one task run on the UI thread. Reported as total occupancy rather
/// than per-task time: many tasks individually too short to look suspicious can
/// still fill the thread and leave no room to service a frame request.
#[inline(always)]
pub fn record_main_thread_task(_duration: Duration) {}

/// Records one window message handled on the UI thread, excluding the paint
/// messages whose cost is already reported as draw time.
#[inline(always)]
pub fn record_window_message(_duration: Duration) {}

/// Records one `Window::draw`, along with how many views it had to re-render.
#[inline(always)]
pub fn record_draw(_duration: Duration, _dirty_views: usize) {}

/// Records one present (scene submission plus swapchain present) and closes
/// out the frame, reporting the digest when the period is up.
#[inline(always)]
pub fn record_present(_duration: Duration, _primitives: usize) {}

/// Records a frame the request-frame throttle dropped before drawing.
#[inline(always)]
pub fn record_throttled() {}

/// Records the pre-draw wait on the compositor's frame-latency handle: time
/// the UI thread spent blocked because the present queue was still full.
#[inline(always)]
pub fn record_gpu_wait(_duration: Duration) {}

/// Records one vsync tick observed by the platform frame pump. `short_wait`
/// means the wait returned early enough that it did not track vblank, so the
/// pump fell back to a timed sleep and the cadence is not display-driven.
#[inline(always)]
pub fn record_vsync_tick(_short_wait: bool) {}

/// Records redraws the frame pump requested from armed windows on one tick.
#[inline(always)]
pub fn record_redraws_requested(_count: usize) {}
