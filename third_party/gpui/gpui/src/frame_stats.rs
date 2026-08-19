//! Per-second digest of frame pacing, emitted at info level while enabled.
//!
//! Collection is off until [`set_enabled`] turns it on, so an ordinary run pays
//! one relaxed atomic load per recorded event and writes nothing. Callers on
//! hot paths that must time an event to report it check [`enabled`] first and
//! skip the clock reads entirely.
//!
//! Per-frame logging cannot diagnose a frame-rate problem at display refresh
//! rates: the lines outrun the log writer and their cost changes the number
//! being measured. Each stage of the render chain instead folds its timing
//! into this accumulator, and one line per second says where the budget went.
//!
//! `arm-lag` separates an idle window from a blocked one: the frame pump
//! clears a window's request as soon as it asks for the redraw, so "nothing
//! armed a frame" and "the UI thread never came back to service one" produce
//! identical tick counts. The time an armed request waited for the UI thread
//! tells them apart.
//!
//! The three counters that localize a bottleneck are `vsync` (ticks the
//! platform pump observed), `req` (redraws it asked windows for), and
//! `frames` (frames the UI thread actually presented). `vsync` below the
//! display refresh rate means the pump parked for lack of demand; `req` below
//! `vsync` means nothing was dirty; `frames` below `req` means the UI thread
//! could not keep up, and the `draw`/`present`/`gpu-wait` splits say which
//! part of it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const REPORT_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_VSYNC_INTERVAL_US: u64 = 16_667;
/// A frame interval past this multiple of the display's is visible as a stutter.
const LONG_FRAME_FACTOR: u32 = 3;

/// Sample count, total and maximum of one timing series, in microseconds.
#[derive(Clone, Copy)]
struct Series {
    count: u64,
    total_us: u64,
    max_us: u64,
}

impl Series {
    const EMPTY: Self = Self {
        count: 0,
        total_us: 0,
        max_us: 0,
    };

    fn add(&mut self, duration: Duration) {
        let us = duration.as_micros() as u64;
        self.count += 1;
        self.total_us += us;
        self.max_us = self.max_us.max(us);
    }

    fn avg_ms(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total_us as f64 / self.count as f64 / 1000.0
        }
    }

    fn max_ms(&self) -> f64 {
        self.max_us as f64 / 1000.0
    }
}

struct Stats {
    window_started: Option<Instant>,
    last_present: Option<Instant>,
    /// When the pending frame request was armed, if one is still unserviced.
    armed_at: Option<Instant>,
    frames: u64,
    throttled: u64,
    long_frames: u64,
    redraws_requested: u64,
    vsync_ticks: u64,
    vsync_short_waits: u64,
    dirty_views: u64,
    primitives: u64,
    draw: Series,
    present: Series,
    gpu_wait: Series,
    interval: Series,
    arm_lag: Series,
    main_tasks: Series,
    window_msgs: Series,
}

impl Stats {
    const fn new() -> Self {
        Self {
            window_started: None,
            last_present: None,
            armed_at: None,
            frames: 0,
            throttled: 0,
            long_frames: 0,
            redraws_requested: 0,
            vsync_ticks: 0,
            vsync_short_waits: 0,
            dirty_views: 0,
            primitives: 0,
            draw: Series::EMPTY,
            present: Series::EMPTY,
            gpu_wait: Series::EMPTY,
            interval: Series::EMPTY,
            arm_lag: Series::EMPTY,
            main_tasks: Series::EMPTY,
            window_msgs: Series::EMPTY,
        }
    }

    /// Reset the counters for a new reporting period, keeping the state of the
    /// frame in flight so the next period still measures its interval and the
    /// latency of a request armed just before the report.
    fn restart(&mut self, now: Instant) {
        let last_present = self.last_present;
        let armed_at = self.armed_at;
        *self = Self::new();
        self.window_started = Some(now);
        self.last_present = last_present;
        self.armed_at = armed_at;
    }
}

static STATS: Mutex<Stats> = Mutex::new(Stats::new());
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Display refresh interval, published by the platform's vsync provider.
static VSYNC_INTERVAL_US: AtomicU64 = AtomicU64::new(DEFAULT_VSYNC_INTERVAL_US);

/// Turn collection and reporting on or off. Counters reset on every change, so
/// a reporting period never mixes samples from either side of the switch and
/// the first interval after enabling is not measured against a stale frame.
pub fn set_enabled(enabled: bool) {
    if ENABLED.swap(enabled, Ordering::Release) == enabled {
        return;
    }
    *STATS.lock().unwrap() = Stats::new();
}

/// Whether collection is on. Hot paths that would need a clock read to report
/// an event check this before taking one.
#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Publish the display refresh interval so the digest can report the frame
/// rate the machine is actually capable of, and classify long frames.
pub fn set_vsync_interval(interval: Duration) {
    VSYNC_INTERVAL_US.store((interval.as_micros() as u64).max(1), Ordering::Relaxed);
}

/// Records a frame request arming, so the next draw can report how long the
/// request waited for the UI thread. Only the oldest unserviced request is
/// kept: a later arm before the draw is coalesced into the same frame.
pub fn record_frame_armed() {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    STATS.lock().unwrap().armed_at.get_or_insert(now);
}

/// Closes out the latency of the frame request the UI thread is now servicing.
/// Called for every serviced request, including ones that present without
/// drawing: a stale arm timestamp left behind by those would charge its whole
/// age to whichever later frame happened to draw.
pub fn record_request_serviced() {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let mut stats = STATS.lock().unwrap();
    if let Some(armed_at) = stats.armed_at.take() {
        stats.arm_lag.add(now.saturating_duration_since(armed_at));
    }
}

/// Records one task run on the UI thread. Reported as total occupancy rather
/// than per-task time: many tasks individually too short to look suspicious can
/// still fill the thread and leave no room to service a frame request.
pub fn record_main_thread_task(duration: Duration) {
    if !enabled() {
        return;
    }
    STATS.lock().unwrap().main_tasks.add(duration);
}

/// Records one window message handled on the UI thread, excluding the paint
/// messages whose cost is already reported as draw time.
pub fn record_window_message(duration: Duration) {
    if !enabled() {
        return;
    }
    STATS.lock().unwrap().window_msgs.add(duration);
}

/// Records one `Window::draw`, along with how many views it had to re-render.
pub fn record_draw(duration: Duration, dirty_views: usize) {
    if !enabled() {
        return;
    }
    let mut stats = STATS.lock().unwrap();
    stats.draw.add(duration);
    stats.dirty_views += dirty_views as u64;
}

/// Records one present (scene submission plus swapchain present) and closes
/// out the frame, reporting the digest when the period is up.
pub fn record_present(duration: Duration, primitives: usize) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let mut stats = STATS.lock().unwrap();
    stats.present.add(duration);
    stats.frames += 1;
    stats.primitives += primitives as u64;
    if let Some(last) = stats.last_present.replace(now) {
        let interval = now.duration_since(last);
        stats.interval.add(interval);
        let vsync_us = VSYNC_INTERVAL_US.load(Ordering::Relaxed);
        if interval.as_micros() as u64 > vsync_us * LONG_FRAME_FACTOR as u64 {
            stats.long_frames += 1;
        }
    }
    report_if_due(&mut stats, now);
}

/// Records a frame the request-frame throttle dropped before drawing.
pub fn record_throttled() {
    if !enabled() {
        return;
    }
    STATS.lock().unwrap().throttled += 1;
}

/// Records the pre-draw wait on the compositor's frame-latency handle: time
/// the UI thread spent blocked because the present queue was still full.
pub fn record_gpu_wait(duration: Duration) {
    if !enabled() {
        return;
    }
    STATS.lock().unwrap().gpu_wait.add(duration);
}

/// Records one vsync tick observed by the platform frame pump. `short_wait`
/// means the wait returned early enough that it did not track vblank, so the
/// pump fell back to a timed sleep and the cadence is not display-driven.
pub fn record_vsync_tick(short_wait: bool) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let mut stats = STATS.lock().unwrap();
    stats.vsync_ticks += 1;
    if short_wait {
        stats.vsync_short_waits += 1;
    }
    report_if_due(&mut stats, now);
}

/// Records redraws the frame pump requested from armed windows on one tick.
pub fn record_redraws_requested(count: usize) {
    if !enabled() {
        return;
    }
    STATS.lock().unwrap().redraws_requested += count as u64;
}

fn report_if_due(stats: &mut Stats, now: Instant) {
    let Some(started) = stats.window_started else {
        stats.window_started = Some(now);
        return;
    };
    let elapsed = now.duration_since(started);
    if elapsed < REPORT_INTERVAL {
        return;
    }

    let seconds = elapsed.as_secs_f64();
    let vsync_hz = 1_000_000.0 / VSYNC_INTERVAL_US.load(Ordering::Relaxed) as f64;
    log::info!(
        "frames {:.1}/s (display {:.1}Hz) | vsync {:.1}/s (short {}) | req {:.1}/s | \
         draw avg {:.2}ms max {:.2}ms | present avg {:.2}ms max {:.2}ms | \
         gpu-wait avg {:.2}ms max {:.2}ms | interval avg {:.2}ms max {:.2}ms | \
         arm-lag avg {:.2}ms max {:.2}ms (n {}) | \
         main-busy tasks {:.0}ms/s (n {}) msgs {:.0}ms/s (n {}) | \
         long {} | throttled {} | views/frame {:.1} | prims/frame {}",
        stats.frames as f64 / seconds,
        vsync_hz,
        stats.vsync_ticks as f64 / seconds,
        stats.vsync_short_waits,
        stats.redraws_requested as f64 / seconds,
        stats.draw.avg_ms(),
        stats.draw.max_ms(),
        stats.present.avg_ms(),
        stats.present.max_ms(),
        stats.gpu_wait.avg_ms(),
        stats.gpu_wait.max_ms(),
        stats.interval.avg_ms(),
        stats.interval.max_ms(),
        stats.arm_lag.avg_ms(),
        stats.arm_lag.max_ms(),
        stats.arm_lag.count,
        stats.main_tasks.total_us as f64 / 1000.0 / seconds,
        stats.main_tasks.count,
        stats.window_msgs.total_us as f64 / 1000.0 / seconds,
        stats.window_msgs.count,
        stats.long_frames,
        stats.throttled,
        stats.dirty_views as f64 / stats.frames.max(1) as f64,
        stats.primitives / stats.frames.max(1),
    );

    stats.restart(now);
}
