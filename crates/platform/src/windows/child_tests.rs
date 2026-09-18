use std::future::poll_fn;
use std::os::windows::io::AsRawHandle;
use std::process::Command;
use std::ptr;
use std::task::Poll;
use std::time::Duration;

use tokio::runtime::Builder;
use tokio::time::timeout;
use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::windows::child::*;

#[test]
pub fn event_is_emitted_when_child_exits() {
    const WAIT_TIMEOUT: Duration = Duration::from_millis(200);

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

    let runtime = Builder::new_current_thread().enable_time().build().unwrap();

    runtime.block_on(async {
        // Register the task before the exit, as the PTY does, so the exit
        // callback has a parked waker to fire.
        assert!(
            poll_fn(|cx| Poll::Ready(child_exit_watcher.poll_exit(cx)))
                .await
                .is_pending()
        );

        child.kill().unwrap();

        timeout(WAIT_TIMEOUT, poll_fn(|cx| child_exit_watcher.poll_exit(cx)))
            .await
            .expect("child exit did not wake the waiting task");
    });

    assert!(child_exit_watcher.exited());
}
