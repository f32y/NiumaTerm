use std::ffi::c_void;
use std::io::Error;
use std::num::NonZeroU32;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use mio::Waker;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Threading::{
    GetProcessId, INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEINWAITTHREAD,
    WT_EXECUTEONLYONCE,
};

use crate::ChildEvent;
use crate::windows::readiness::SoftReady;

/// Context handed to the WinAPI wait callback. The exit event is delivered over a
/// `std::sync::mpsc` channel (mio 1.2 has no pollable channel); the soft-ready handle
/// wakes the event loop's `Poll` so it re-checks the receiver.
struct CallbackCtx {
    event_tx: Sender<ChildEvent>,
    soft: SoftReady,
}

/// WinAPI callback to run when child process exits.
extern "system" fn child_exit_callback(ctx: *mut c_void, timed_out: bool) {
    if timed_out {
        return;
    }

    // Borrow only: the watcher owns the context and frees it in Drop, after a
    // blocking UnregisterWaitEx has excluded any in-flight callback. Taking
    // ownership here would leak the box whenever the child outlives the
    // watcher (the callback never fires, nobody frees the allocation).
    let ctx = unsafe { &*(ctx as *const CallbackCtx) };
    let _ = ctx.event_tx.send(ChildEvent::Exited);
    ctx.soft.set_ready();
}

/// Owns `child_handle`: the process handle is closed on drop, so callers must
/// hand over a handle (or a duplicate) they will not close themselves.
pub struct ChildExitWatcher {
    wait_handle: AtomicPtr<c_void>,
    event_rx: Receiver<ChildEvent>,
    soft: SoftReady,
    child_handle: HANDLE,
    ctx: *mut CallbackCtx,
    pid: Option<NonZeroU32>,
}

// HANDLE is not Send, so Send is not derived automatically for ChildExitWatcher, but raw pointers
// are generally safe to send between threads as long as the type they deference to is Send, which
// c_void is. (see https://doc.rust-lang.org/nomicon/send-and-sync.html).
unsafe impl Send for ChildExitWatcher {}

impl ChildExitWatcher {
    pub fn new(child_handle: HANDLE) -> Result<ChildExitWatcher, Error> {
        let (event_tx, event_rx) = channel::<ChildEvent>();
        let soft = SoftReady::new();

        let mut wait_handle: HANDLE = ptr::null_mut();
        let ctx = Box::into_raw(Box::new(CallbackCtx {
            event_tx,
            soft: soft.clone(),
        }));

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
            let pid = unsafe { NonZeroU32::new(GetProcessId(child_handle)) };
            Ok(ChildExitWatcher {
                wait_handle: AtomicPtr::from(wait_handle),
                event_rx,
                soft,
                child_handle,
                ctx,
                pid,
            })
        }
    }

    pub fn event_rx(&self) -> &Receiver<ChildEvent> {
        &self.event_rx
    }

    /// The soft-ready handle, so the `Pty` can inject the loop `Waker` at
    /// `register()` time and surface child-exit through `drain_ready()`.
    pub fn soft(&self) -> &SoftReady {
        &self.soft
    }

    /// Install the event loop's waker so child exit wakes the `Poll`.
    pub fn set_waker(&self, waker: Arc<Waker>) {
        self.soft.set_waker(waker);
    }

    pub fn raw_handle(&self) -> HANDLE {
        self.child_handle
    }

    pub fn pid(&self) -> Option<NonZeroU32> {
        self.pid
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

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle;
    use std::process::Command;
    use std::time::Duration;

    use mio::{Events, Poll, Token};
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    use super::*;

    #[test]
    pub fn event_is_emitted_when_child_exits() {
        const WAIT_TIMEOUT: Duration = Duration::from_millis(200);
        const WAKER_TOKEN: Token = Token(0);

        let mut child = Command::new("cmd.exe").spawn().unwrap();

        // The watcher owns (and closes) the handle it is given, while
        // std::process::Child closes its own on drop — hand over a duplicate
        // so the handle is not closed twice.
        let mut dup: HANDLE = ptr::null_mut();
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                child.as_raw_handle() as HANDLE,
                GetCurrentProcess(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        assert_ne!(duplicated, 0);
        let child_exit_watcher = ChildExitWatcher::new(dup).unwrap();

        // The child-exit channel is a plain `std::sync::mpsc`, so the loop is woken
        // through the `Waker` instead of a pollable channel.
        let mut poll = Poll::new().unwrap();
        let waker = Arc::new(Waker::new(poll.registry(), WAKER_TOKEN).unwrap());
        child_exit_watcher.set_waker(waker);

        child.kill().unwrap();

        // Poll for the wakeup or fail with timeout if nothing has been sent.
        let mut events = Events::with_capacity(1);
        poll.poll(&mut events, Some(WAIT_TIMEOUT)).unwrap();
        assert_eq!(events.iter().next().unwrap().token(), WAKER_TOKEN);
        assert!(child_exit_watcher.soft().is_ready());
        // Verify that at least one `ChildEvent::Exited` was received.
        assert_eq!(
            child_exit_watcher.event_rx().try_recv(),
            Ok(ChildEvent::Exited)
        );
    }
}
