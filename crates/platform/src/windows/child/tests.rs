use std::os::windows::io::AsRawHandle;
use std::process::Command;
use std::time::Duration;

use mio::{Events, Poll, Token};
use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::windows::child::*;

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
