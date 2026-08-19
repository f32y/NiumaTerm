//! Demand-driven frame pump: windows arm a per-window frame request when
//! they need a frame; the vsync thread parks on the wake event while no
//! request is armed, so an idle process performs zero periodic wakeups.
//!
//! Race edges deliberately bias toward an extra frame (arming when already
//! armed is a no-op; a signal landing during the awake phase yields at most
//! one spurious wakeup) — a missed frame would freeze the UI, an extra one
//! costs a redraw.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::frame_stats;
use parking_lot::RwLock;
use smallvec::SmallVec;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject};

use crate::SafeHwnd;

pub(crate) struct FramePump {
    /// Auto-reset wake event (raw handle; lives for the process).
    wake_event: isize,
    /// Per-window armed flags.
    requests: RwLock<SmallVec<[(SafeHwnd, Arc<AtomicBool>); 4]>>,
}

// The raw event handle is only used through thread-safe syscalls.
unsafe impl Send for FramePump {}
unsafe impl Sync for FramePump {}

impl FramePump {
    pub(crate) fn new() -> Self {
        let event = unsafe { CreateEventW(None, false, false, None) }
            .expect("failed to create frame pump wake event");
        Self {
            wake_event: event.0 as isize,
            requests: RwLock::new(SmallVec::new()),
        }
    }

    /// Register a window, returning its armed flag. Starts armed so a fresh
    /// window gets its first frames.
    pub(crate) fn register(&self, hwnd: HWND) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(true));
        self.requests.write().push((hwnd.into(), flag.clone()));
        self.wake();
        flag
    }

    pub(crate) fn unregister(&self, hwnd: HWND) {
        self.requests.write().retain(|(h, _)| h.as_raw() != hwnd);
        // Wake a parked pump so it re-evaluates its window set.
        self.wake();
    }

    /// A thread-safe waker arming `flag`; cheap when already armed (no event
    /// signal).
    pub(crate) fn waker(self: &Arc<Self>, flag: Arc<AtomicBool>) -> Arc<dyn Fn() + Send + Sync> {
        let pump = Arc::clone(self);
        Arc::new(move || {
            if !flag.swap(true, Ordering::AcqRel) {
                frame_stats::record_frame_armed();
                pump.wake();
            }
        })
    }

    /// Arm a window's flag directly (UI-thread paths like DirectManipulation
    /// sustain).
    pub(crate) fn arm(&self, flag: &AtomicBool) {
        if !flag.swap(true, Ordering::AcqRel) {
            frame_stats::record_frame_armed();
            self.wake();
        }
    }

    pub(crate) fn wake(&self) {
        unsafe {
            let _ = SetEvent(HANDLE(self.wake_event as _));
        }
    }

    pub(crate) fn any_armed(&self) -> bool {
        self.requests
            .read()
            .iter()
            .any(|(_, flag)| flag.load(Ordering::Acquire))
    }

    /// Drain the armed windows, clearing each flag *before* the caller
    /// redraws so paint-time notifications arm the next frame.
    pub(crate) fn take_armed(&self) -> SmallVec<[SafeHwnd; 4]> {
        self.requests
            .read()
            .iter()
            .filter(|(_, flag)| flag.swap(false, Ordering::AcqRel))
            .map(|(hwnd, _)| *hwnd)
            .collect()
    }

    /// Block until any waker signals (auto-reset event).
    pub(crate) fn park(&self) {
        unsafe {
            WaitForSingleObject(HANDLE(self.wake_event as _), INFINITE);
        }
    }
}
