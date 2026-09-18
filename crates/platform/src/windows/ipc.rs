pub use crate::ipc_message::MAX_MESSAGE_BYTES;

use std::io::{self, Write};
use std::time::{Duration, Instant};
use std::{fs, ptr, thread};

use tokio::net::windows::named_pipe::ServerOptions;
use tracing::warn;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;

use crate::ipc_message::read_message;

const MUTEX_NAME: &str = "Local\\NiumaTerm.SingleInstance";
const PIPE_NAME: &str = "\\\\.\\pipe\\NiumaTerm.Ipc";
const TESTING_MUTEX_NAME: &str = "Local\\NiumaTerm.Testing.Ipc";
const TESTING_PIPE_NAME: &str = "\\\\.\\pipe\\NiumaTerm.Testing.Ipc";

fn mutex_name(testing: bool) -> &'static str {
    if testing {
        TESTING_MUTEX_NAME
    } else {
        MUTEX_NAME
    }
}

fn pipe_name(testing: bool) -> &'static str {
    if testing {
        TESTING_PIPE_NAME
    } else {
        PIPE_NAME
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

/// Try to become the primary instance. The mutex handle intentionally lives
/// until process exit.
pub fn try_become_primary(testing: bool) -> bool {
    let name = wide(mutex_name(testing));

    unsafe {
        let handle = CreateMutexW(ptr::null(), 1, name.as_ptr());

        if handle.is_null() {
            warn!(
                "CreateMutexW failed ({}); skipping single-instance",
                GetLastError()
            );

            return true;
        }

        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Send one UTF-8 line to the primary process, retrying until `timeout` elapses.
pub fn send(message: &str, timeout: Duration, testing: bool) -> io::Result<()> {
    let deadline = Instant::now() + timeout;

    loop {
        match fs::OpenOptions::new().write(true).open(pipe_name(testing)) {
            Ok(mut pipe) => return writeln!(pipe, "{message}"),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(error),
        }
    }
}

/// Run the primary process pipe server. Returning `false` from the callback
/// stops the server. The callback runs on the shared runtime and must return
/// promptly.
pub fn spawn_server(
    testing: bool,
    on_message: impl FnMut(Vec<u8>) -> bool + Send + 'static,
) -> io::Result<()> {
    nmt_runtime::handle().spawn(serve_pipe(testing, on_message));

    Ok(())
}

async fn serve_pipe(testing: bool, mut on_message: impl FnMut(Vec<u8>) -> bool) {
    let name = pipe_name(testing);

    loop {
        // Each client gets a fresh instance; a sender that finds none waiting
        // retries until the next one is created.
        let server = match ServerOptions::new()
            .access_outbound(false)
            .in_buffer_size(512)
            .out_buffer_size(512)
            .create(name)
        {
            Ok(server) => server,
            Err(error) => {
                warn!("creating the IPC pipe failed ({error}); IPC disabled");

                return;
            }
        };

        if server.connect().await.is_err() {
            continue;
        }

        let Some(bytes) = read_message(server).await else {
            continue;
        };

        if !on_message(bytes) {
            return;
        }
    }
}
