use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::{env, mem, thread};

use tracing::warn;

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Per-user directory holding the socket and the single-instance lock.
///
/// `XDG_RUNTIME_DIR` is already private to one user; the temp directory is
/// not on every system, so the uid is part of the name and the directory is
/// created with owner-only access. A socket path also has to fit in
/// `sockaddr_un`, which is why this stays a short name directly under the
/// runtime directory rather than a nested app path.
fn runtime_dir() -> io::Result<PathBuf> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(|value| -> PathBuf { value.into() })
        .filter(|path| path.is_absolute())
        .unwrap_or_else(env::temp_dir);

    // SAFETY: `getuid` reads process state and cannot fail.
    let uid = unsafe { libc::getuid() };

    let directory = base.join(format!("NiumaTerm-{uid}"));

    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;

    Ok(directory)
}

fn socket_path(testing: bool) -> io::Result<PathBuf> {
    let name = if testing { "testing.sock" } else { "ipc.sock" };

    Ok(runtime_dir()?.join(name))
}

fn lock_path(testing: bool) -> io::Result<PathBuf> {
    let name = if testing { "testing.lock" } else { "ipc.lock" };

    Ok(runtime_dir()?.join(name))
}

/// Try to become the primary instance.
///
/// The exclusive lock lives as long as the file descriptor, and the kernel
/// releases it when the process exits — including a crash, which is what
/// keeps a stale lock from blocking the next launch. The descriptor is
/// therefore leaked on purpose, the way the Windows mutex handle is.
pub fn try_become_primary(testing: bool) -> bool {
    let path = match lock_path(testing) {
        Ok(path) => path,

        Err(error) => {
            warn!("IPC lock directory unavailable ({error}); skipping single-instance");

            return true;
        }
    };

    let file = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file,

        Err(error) => {
            warn!("IPC lock file unavailable ({error}); skipping single-instance");

            return true;
        }
    };

    // SAFETY: the descriptor is owned by `file` and stays open for the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        mem::forget(file);

        return true;
    }

    let error = io::Error::last_os_error();

    if matches!(
        error.raw_os_error(),
        Some(libc::EWOULDBLOCK) | Some(libc::EINTR)
    ) {
        return false;
    }

    warn!("flock failed ({error}); skipping single-instance");

    true
}

/// Send one UTF-8 line to the primary process, retrying until `timeout` elapses.
pub fn send(message: &str, timeout: Duration, testing: bool) -> io::Result<()> {
    let path = socket_path(testing)?;
    let deadline = Instant::now() + timeout;

    loop {
        match UnixStream::connect(&path) {
            Ok(mut stream) => return writeln!(stream, "{message}"),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(error),
        }
    }
}

/// Run the primary process socket server. Returning `false` from the callback
/// stops the server thread.
pub fn spawn_server(
    testing: bool,
    on_message: impl FnMut(Vec<u8>) -> bool + Send + 'static,
) -> io::Result<()> {
    let path = socket_path(testing)?;

    // Only the instance that won the single-instance lock reaches here, so a
    // socket file left behind by a crashed predecessor has no live listener
    // and binding would otherwise fail with `EADDRINUSE` forever.
    if let Err(error) = fs::remove_file(&path)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error);
    }

    let listener = UnixListener::bind(&path)?;

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

    thread::Builder::new()
        .name("nmt-ipc".into())
        .spawn(move || serve_socket(listener, on_message))
        .map(|_| ())
}

fn serve_socket(listener: UnixListener, mut on_message: impl FnMut(Vec<u8>) -> bool) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };

        let mut bytes = Vec::new();

        if Read::take(stream, (MAX_MESSAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > MAX_MESSAGE_BYTES
        {
            continue;
        }

        if !on_message(bytes) {
            return;
        }
    }
}
