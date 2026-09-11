use std::time::{Duration, Instant};
use std::{io, thread};

/// The interval between liveness checks.
///
/// There is no portable way to wait on a process that is not this one's child,
/// so the state is polled. The interval trades a bounded delay after the
/// process goes away against the cost of asking, which is one failing signal.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Block until the process with `pid` has exited, and report whether it had by
/// the time this returned.
///
/// A process that cannot be signalled is reported as gone: the identifier is
/// either already free or belongs to something this account may not touch, and
/// neither is worth waiting on. `timeout` bounds the opposite case, a process
/// that never finishes shutting down, so a caller waiting for a predecessor is
/// delayed rather than stuck behind it.
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };

    let deadline = Instant::now() + timeout;

    loop {
        // Signal zero performs the permission and existence checks without
        // delivering anything, which is the supported way to ask.
        // SAFETY: both arguments are plain integers.
        if unsafe { libc::kill(pid, 0) } != 0 {
            // `EPERM` means it is alive but not ours; anything else means it
            // is gone.
            return io::Error::last_os_error().raw_os_error() != Some(libc::EPERM);
        }

        if Instant::now() >= deadline {
            return false;
        }

        thread::sleep(POLL_INTERVAL);
    }
}
