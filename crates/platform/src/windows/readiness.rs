//! Per-source soft readiness.
//!
//! mio 1.2 removed `Registration`/`SetReadiness` (mio 0.6's user-space readiness).
//! ConPTY input writes and child exit complete outside the loop's poll set.
//! Their wait callbacks tell the event loop when the source can make progress.
//! Output reads use mio's IOCP directly and retain partial-read readiness locally.
//!
//! This is the minimal faithful replacement: one `AtomicBool` flag per source plus a
//! `Waker` (the event loop's), injected at `register()` time rather than construction
//! (the `Pty` and its pipes exist before the loop's `Poll`/`Waker` do). A flag
//! set before the waker is installed simply stays set, so the first poll after register
//! observes it — no lost wakeup. The flag stays set until the source has no pending
//! work, or until a write must wait for native completion.
//!
//! The waker, however, only fires on the clear->set edge (the callback calls `set_ready`
//! only when the flag was clear). The event loop also checks `has_ready()` before
//! blocking so pending child exit and partially consumed output stay observable.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mio::Waker;
use parking_lot::Mutex;

#[derive(Clone, Default)]
pub struct SoftReady {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    ready: AtomicBool,
    waker: Mutex<Option<Arc<Waker>>>,
}

impl SoftReady {
    pub fn new() -> Self {
        Self::default()
    }

    /// Completion side: mark this source ready and wake the loop's `Poll`.
    /// If no waker is installed yet (pre-`register`), the flag is still set and a
    /// later poll picks it up.
    pub fn set_ready(&self) {
        self.inner.ready.store(true, Ordering::SeqCst);

        if let Some(waker) = self.inner.waker.lock().as_ref() {
            // A failed wake just means the `Poll` is gone; the source is tearing down.
            let _ = waker.wake();
        }
    }

    /// Loop side: clear the flag. Call only once the source's buffer is fully drained
    /// (keeps the flag level-like).
    pub fn clear(&self) {
        self.inner.ready.store(false, Ordering::SeqCst);
    }

    pub fn is_ready(&self) -> bool {
        self.inner.ready.load(Ordering::SeqCst)
    }

    /// `register()` time: install the event loop's waker so future `set_ready` calls
    /// wake the `Poll`.
    pub fn set_waker(&self, waker: Arc<Waker>) {
        *self.inner.waker.lock() = Some(waker);
    }
}
