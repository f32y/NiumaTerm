//! Per-source soft readiness.
//!
//! mio 1.2 removed `Registration`/`SetReadiness` (mio 0.6's user-space readiness).
//! The ConPTY anon pipes have no real OS readiness source — a worker thread does a
//! blocking `ReadFile`/`WriteFile` and must tell the event loop "this source has data".
//!
//! This is the minimal faithful replacement: one `AtomicBool` flag per source plus a
//! `Waker` (the event loop's), injected at `register()` time rather than construction
//! (the `Pty` and its worker threads exist before the loop's `Poll`/`Waker` do). A flag
//! set before the waker is installed simply stays set, so the first poll after register
//! observes it — no lost wakeup. The flag is level-like: it stays set until the source's
//! buffer is fully drained.
//!
//! The waker, however, only fires on the clear->set edge (the worker calls `set_ready`
//! only when the flag was clear). A consumer that stops draining early (e.g. `pty_read`
//! capped by `MAX_LOCKED_READ`) leaves the flag set with data still buffered and gets no
//! further wakeup. The event loop closes this gap by checking `has_ready()` before it
//! blocks in `poll()` and using a zero timeout when a source is still ready, so the
//! level state is re-observed instead of slept on.

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

    /// Worker-thread side: mark this source ready and wake the loop's `Poll`.
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
