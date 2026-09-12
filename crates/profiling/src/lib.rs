//! Performance collection shared by application and rendering code.
//! Without `--cfg enable_profiling`, hot-path hooks carry no sampling state.

pub mod allocation;
pub mod frame_stats;
#[cfg(enable_profiling)]
pub mod pty;
pub mod transcript;

/// Enable runtime-gated frame, transcript, and allocation collection together.
#[inline]
pub fn set_enabled(enabled: bool) {
    frame_stats::set_enabled(enabled);
    allocation::set_enabled(enabled);
}

/// Whether runtime-gated collection is currently active.
#[inline(always)]
pub fn enabled() -> bool {
    frame_stats::enabled()
}

#[cfg(all(test, enable_profiling))]
pub(crate) static TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
