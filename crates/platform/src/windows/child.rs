#[cfg(test)]
#[path = "child_tests.rs"]
mod child_tests;

use std::ffi::c_void;
use std::io::Error;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::task::{Context, Poll};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Threading::{WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE};

use crate::windows::readiness::SoftReady;
use crate::windows::registered_wait::RegisteredWait;

/// WinAPI callback to run when child process exits.
unsafe extern "system" fn child_exit_callback(ctx: *mut c_void, timed_out: bool) {
    if timed_out {
        return;
    }

    // Borrow only: the registration owns the context and frees it after a
    // blocking unregister has excluded any in-flight callback.
    let soft = unsafe { &*(ctx as *const SoftReady) };

    // The flag records the exit before any task registers, and waking the
    // registered task lets it observe the flag.
    soft.set_ready();
}

/// Owns `child_handle`: the process handle is closed on drop, so callers must
/// hand over a handle (or a duplicate) they will not close themselves.
pub struct ChildExitWatcher {
    // Declared first: the wait is unregistered before the process handle it
    // watches is closed.
    wait: RegisteredWait<SoftReady>,
    _child_handle: OwnedHandle,
}

impl ChildExitWatcher {
    pub fn new(child_handle: HANDLE) -> Result<ChildExitWatcher, Error> {
        // Taken first so a failed registration still closes the handle.
        let child_handle = unsafe { OwnedHandle::from_raw_handle(child_handle) };

        let wait = RegisteredWait::new(
            child_handle.as_raw_handle(),
            SoftReady::new(),
            child_exit_callback,
            WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
        )?;

        Ok(ChildExitWatcher {
            wait,
            _child_handle: child_handle,
        })
    }

    /// Reports whether the child has exited without waiting.
    pub fn exited(&self) -> bool {
        self.wait.context().is_ready()
    }

    /// Complete once the child exits. The waker is installed before the check
    /// so an exit reported between the two still wakes the task.
    pub fn poll_exit(&self, cx: &mut Context<'_>) -> Poll<()> {
        self.wait.context().register_task_waker(cx.waker());

        if self.exited() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
