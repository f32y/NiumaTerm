//! Readiness for native write and child-exit callbacks.
//!
//! The persistent flag retains completion state until the owner consumes it
//! and lets a callback wake only on the clear-to-set edge. Async consumers
//! install their waker before checking the operation's result, so a
//! concurrent completion cannot be lost between checking and suspending.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Waker;

use futures::task::AtomicWaker;

#[derive(Clone, Default)]
pub struct SoftReady {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    ready: AtomicBool,
    task_waker: AtomicWaker,
}

impl SoftReady {
    pub fn new() -> Self {
        Self::default()
    }

    /// Completion side: mark this source ready and wake the registered task.
    /// If no task is registered yet, the flag is still set and the first
    /// check after registration observes it.
    pub fn set_ready(&self) {
        self.inner.ready.store(true, Ordering::SeqCst);

        self.inner.task_waker.wake();
    }

    /// Owner side: clear the flag once the source has no completed work left.
    pub fn clear(&self) {
        self.inner.ready.store(false, Ordering::SeqCst);
    }

    pub fn is_ready(&self) -> bool {
        self.inner.ready.load(Ordering::SeqCst)
    }

    /// Install before checking completion so a concurrent callback cannot be lost.
    pub fn register_task_waker(&self, waker: &Waker) {
        self.inner.task_waker.register(waker);
    }
}
