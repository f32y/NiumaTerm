#[cfg(test)]
#[path = "child_tests.rs"]
mod child_tests;

use std::ffi::c_void;
use std::io::Error;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::task::{Context, Poll};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Threading::{
    INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEINWAITTHREAD,
    WT_EXECUTEONLYONCE,
};

use crate::windows::readiness::SoftReady;

/// WinAPI callback to run when child process exits.
extern "system" fn child_exit_callback(ctx: *mut c_void, timed_out: bool) {
    if timed_out {
        return;
    }

    // Borrow only: the watcher owns the context and frees it in Drop, after a
    // blocking UnregisterWaitEx has excluded any in-flight callback. Taking
    // ownership here would leak the box whenever the child outlives the
    // watcher (the callback never fires, nobody frees the allocation).
    let soft = unsafe { &*(ctx as *const SoftReady) };

    // The flag records the exit before any task registers, and waking the
    // registered task lets it observe the flag.
    soft.set_ready();
}

/// Owns `child_handle`: the process handle is closed on drop, so callers must
/// hand over a handle (or a duplicate) they will not close themselves.
pub struct ChildExitWatcher {
    wait_handle: AtomicPtr<c_void>,
    soft: SoftReady,
    child_handle: HANDLE,
    ctx: *mut SoftReady,
}

// HANDLE is not Send, so Send is not derived automatically for ChildExitWatcher, but raw pointers
// are generally safe to send between threads as long as the type they deference to is Send, which
// c_void is. (see https://doc.rust-lang.org/nomicon/send-and-sync.html).
unsafe impl Send for ChildExitWatcher {}

impl ChildExitWatcher {
    pub fn new(child_handle: HANDLE) -> Result<ChildExitWatcher, Error> {
        let soft = SoftReady::new();

        let mut wait_handle: HANDLE = ptr::null_mut();

        let ctx = Box::into_raw(Box::new(soft.clone()));

        let success = unsafe {
            RegisterWaitForSingleObject(
                &mut wait_handle,
                child_handle,
                Some(child_exit_callback),
                ctx.cast(),
                INFINITE,
                WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
            )
        };

        if success == 0 {
            let err = Error::last_os_error();

            // No wait was registered, so the context box and the process
            // handle we own are reclaimed here or never.
            unsafe {
                drop(Box::from_raw(ctx));

                CloseHandle(child_handle);
            }

            Err(err)
        } else {
            Ok(ChildExitWatcher {
                wait_handle: wait_handle.into(),
                soft,
                child_handle,
                ctx,
            })
        }
    }

    /// Reports whether the child has exited without waiting.
    pub fn exited(&self) -> bool {
        self.soft.is_ready()
    }

    /// Complete once the child exits. The waker is installed before the check
    /// so an exit reported between the two still wakes the task.
    pub fn poll_exit(&self, cx: &mut Context<'_>) -> Poll<()> {
        self.soft.register_task_waker(cx.waker());

        if self.exited() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for ChildExitWatcher {
    fn drop(&mut self) {
        unsafe {
            // Blocking unregister (INVALID_HANDLE_VALUE): waits for any
            // in-flight callback to finish, which is what makes freeing the
            // context box and closing the process handle below safe. Never
            // runs on the wait-callback thread, so it cannot self-deadlock.
            UnregisterWaitEx(
                self.wait_handle.load(Ordering::Relaxed) as HANDLE,
                INVALID_HANDLE_VALUE,
            );

            drop(Box::from_raw(self.ctx));

            CloseHandle(self.child_handle);
        }
    }
}
