use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::CreateMutexW;

const MUTEX_NAME: &str = "Local\\NiumaTerm.SingleInstance";
const PIPE_NAME: &str = "\\\\.\\pipe\\NiumaTerm.Ipc";
const TESTING_MUTEX_NAME: &str = "Local\\NiumaTerm.Testing.Ipc";
const TESTING_PIPE_NAME: &str = "\\\\.\\pipe\\NiumaTerm.Testing.Ipc";
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

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
        let handle = CreateMutexW(std::ptr::null(), 1, name.as_ptr());

        if handle.is_null() {
            tracing::warn!(
                "CreateMutexW failed ({}); skipping single-instance",
                GetLastError()
            );

            return true;
        }

        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Send one UTF-8 line to the primary process, retrying until `timeout` elapses.
pub fn send(message: &str, timeout: Duration, testing: bool) -> std::io::Result<()> {
    let deadline = Instant::now() + timeout;

    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .open(pipe_name(testing))
        {
            Ok(mut pipe) => return writeln!(pipe, "{message}"),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(error),
        }
    }
}

/// Run the primary process pipe server. Returning `false` from the callback
/// stops the server thread.
pub fn spawn_server(testing: bool, mut on_message: impl FnMut(Vec<u8>) -> bool + Send + 'static) {
    std::thread::Builder::new()
        .name("nmt-ipc".into())
        .spawn(move || {
            let name = wide(pipe_name(testing));

            loop {
                let handle = unsafe {
                    CreateNamedPipeW(
                        name.as_ptr(),
                        PIPE_ACCESS_INBOUND,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        PIPE_UNLIMITED_INSTANCES,
                        512,
                        512,
                        0,
                        std::ptr::null(),
                    )
                };

                if handle == INVALID_HANDLE_VALUE {
                    tracing::warn!("CreateNamedPipeW failed ({}); IPC disabled", unsafe {
                        GetLastError()
                    });
                    return;
                }

                let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0
                    || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;

                let mut pipe = unsafe { File::from_raw_handle(handle as _) };

                if !connected {
                    continue;
                }

                let mut bytes = Vec::new();

                if Read::take(&mut pipe, (MAX_MESSAGE_BYTES + 1) as u64)
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
        })
        .expect("spawn nmt-ipc thread");
}
