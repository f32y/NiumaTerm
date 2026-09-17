//! Overlapped ConPTY pipes.
//!
//! `CreatePipe` cannot open an end for overlapped I/O, which forces a blocked
//! thread per direction. Each pair here is a single-instance named pipe
//! instead: the end this process keeps is overlapped, and the end handed to
//! the console host stays synchronous, which is what the host expects.
//!
//! Output reads use mio's named pipe, so completions arrive directly at the
//! registered poller's IOCP. Input writes complete against an auto-reset event;
//! its wait callback marks [`SoftReady`] and wakes the loop. Writes can also
//! settle on demand without a poller.

#[cfg(test)]
#[path = "pipes_tests.rs"]
mod pipes_tests;

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{io, mem, process, ptr};

use mio::windows::NamedPipe;
use mio::{Interest, Poll, Token};
use windows_sys::Win32::Foundation::{
    ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PeekNamedPipe,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEINWAITTHREAD,
};

use crate::windows::readiness::SoftReady;
use crate::windows::win32_string;

/// Kernel buffer of each pipe. The console host
/// writes in 4 KiB pieces; room for several lets it keep producing while the
/// event loop parses, and lets one read collect whatever accumulated meanwhile.
const PIPE_BUFFER: usize = 64 * 1024;

/// Upper bound of one native write. The bytes are copied because an overlapped
/// write needs memory that outlives the call, so a large paste is submitted in
/// pieces rather than duplicated whole.
const WRITE_CHUNK: usize = 64 * 1024;

static NEXT_PIPE: AtomicU32 = AtomicU32::new(0);

/// Create the pipe carrying console output: the overlapped end read here, and
/// the synchronous write end for the console host.
pub(crate) fn conout_pair() -> io::Result<(ConoutPipe, OwnedHandle)> {
    let (ours, theirs) = pipe_pair(PIPE_ACCESS_INBOUND, GENERIC_WRITE)?;

    Ok((ConoutPipe::new(ours), theirs))
}

/// Create the pipe carrying console input: the synchronous read end for the
/// console host, and the overlapped end written here.
pub(crate) fn conin_pair() -> io::Result<(OwnedHandle, ConinPipe)> {
    let (ours, theirs) = pipe_pair(PIPE_ACCESS_OUTBOUND, GENERIC_READ)?;

    Ok((theirs, ConinPipe::new(ours)?))
}

/// One connected pipe: the overlapped end with `access`, then the synchronous
/// end opened with `peer_access`.
///
/// The name only has to be unique on this machine. `FIRST_PIPE_INSTANCE` makes
/// creation fail if another process already owns the name, and the single
/// instance is taken by the open below, so the pipe can never gain a second
/// peer: if another process connects first, the open fails and no terminal
/// data is ever written to it.
pub(super) fn pipe_pair(access: u32, peer_access: u32) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());

    let name = win32_string(&format!(
        r"\\.\pipe\nmt-conpty.{}.{}.{nanos}",
        process::id(),
        NEXT_PIPE.fetch_add(1, Ordering::Relaxed),
    ));

    let ours = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            access | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER as u32,
            PIPE_BUFFER as u32,
            0,
            ptr::null(),
        )
    };

    if ours == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    let ours = unsafe { OwnedHandle::from_raw_handle(ours) };

    let theirs = unsafe {
        CreateFileW(
            name.as_ptr(),
            peer_access,
            0,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };

    if theirs == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    Ok((ours, unsafe { OwnedHandle::from_raw_handle(theirs) }))
}

/// Runs on the operating system's wait thread when an operation completes.
extern "system" fn operation_completed(ctx: *mut c_void, _timed_out: bool) {
    // Borrow only: the end owns the box and frees it after a blocking
    // unregister has excluded any callback still running.
    let soft = unsafe { &*(ctx as *const SoftReady) };

    // An operation that finished inside its own call signals the event too.
    // Waking only on the clear-to-set edge keeps a burst of those from posting
    // one wakeup per chunk.
    if !soft.is_ready() {
        soft.set_ready();
    }
}

/// The overlapped input end and the single write it runs at a time.
struct PipeEnd {
    handle: OwnedHandle,

    /// Boxed so the address the kernel holds stays valid when this moves.
    overlapped: Box<OVERLAPPED>,

    /// Auto-reset, so the registered wait fires once per completion. Held so
    /// the handle inside `overlapped` stays open for as long as it is used.
    _event: OwnedHandle,

    wait: HANDLE,

    /// Boxed because the wait callback keeps its address.
    soft: Box<SoftReady>,

    in_flight: bool,
}

// The raw pointers refer to allocations this value owns, and the wait callback
// only touches the thread-safe `SoftReady`.
unsafe impl Send for PipeEnd {}

impl PipeEnd {
    fn new(handle: OwnedHandle) -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };

        if event.is_null() {
            return Err(io::Error::last_os_error());
        }

        let event = unsafe { OwnedHandle::from_raw_handle(event) };

        let soft = Box::new(SoftReady::new());

        let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { mem::zeroed() });

        overlapped.hEvent = event.as_raw_handle();

        let mut wait: HANDLE = ptr::null_mut();

        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                event.as_raw_handle(),
                Some(operation_completed),
                ptr::from_ref::<SoftReady>(&soft).cast(),
                INFINITE,
                WT_EXECUTEINWAITTHREAD,
            )
        };

        if registered == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            handle,
            overlapped,
            _event: event,
            wait,
            soft,
            in_flight: false,
        })
    }

    /// Turn the return of `WriteFile` into a started operation. An
    /// immediate success still reports its byte count through `finished`.
    fn started(&mut self, succeeded: i32) -> io::Result<()> {
        if succeeded == 0 {
            let error = io::Error::last_os_error();

            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
        }

        self.in_flight = true;

        Ok(())
    }

    fn result(&mut self) -> io::Result<Option<usize>> {
        let mut transferred = 0;

        let done = unsafe {
            GetOverlappedResult(
                self.handle.as_raw_handle(),
                &*self.overlapped,
                &mut transferred,
                0,
            )
        };

        if done != 0 {
            self.in_flight = false;

            return Ok(Some(transferred as usize));
        }

        let error = io::Error::last_os_error();

        if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
            return Ok(None);
        }

        self.in_flight = false;

        Err(error)
    }

    /// The byte count of the running operation, or `None` while it is pending.
    ///
    /// Reporting "pending" clears the ready flag, and the completion may have
    /// raised it just before. Looking again after the clear closes that gap; a
    /// completion found there raises the flag again because its own wakeup may
    /// already be spent.
    fn finished(&mut self) -> io::Result<Option<usize>> {
        if let Some(transferred) = self.result()? {
            return Ok(Some(transferred));
        }

        self.soft.clear();

        let finished = self.result()?;

        if finished.is_some() {
            self.soft.set_ready();
        }

        Ok(finished)
    }
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        unsafe {
            // Blocking unregister: no callback can touch `soft` afterwards, and
            // nothing else consumes the event the wait below may need.
            UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE);

            if self.in_flight {
                // The kernel still references `overlapped` and the owner's
                // buffer. Both are freed after this returns, so the cancelled
                // operation has to be seen through to its completion first.
                CancelIoEx(self.handle.as_raw_handle(), &*self.overlapped);

                let mut transferred = 0;

                GetOverlappedResult(
                    self.handle.as_raw_handle(),
                    &*self.overlapped,
                    &mut transferred,
                    1,
                );
            }
        }
    }
}

/// The console output stream, driven by the registered poller's IOCP. `read`
/// returns `Ok(0)` while no output is waiting and `BrokenPipe` after hangup.
pub struct ConoutPipe {
    // Mio 1.2.2 uses a 4 KiB internal read buffer with no public size setting.
    // PIPE_BUFFER only sizes the kernel buffer; larger caller read slices do
    // not enlarge mio's buffer. Direct IOCP removes the wait-thread hop, but
    // the smaller reads increase completion and poll frequency during sustained
    // output and can reduce throughput. Using 64 KiB internal reads requires
    // a mio dependency patch; its effect on throughput and short-message
    // latency needs measurement.
    pipe: NamedPipe,

    /// A successful read can leave data in mio's buffer without another edge.
    /// Keep the loop reading until it observes `WouldBlock`.
    readable: bool,
}

impl ConoutPipe {
    fn new(handle: OwnedHandle) -> Self {
        Self {
            // SAFETY: The connected handle was opened for overlapped I/O and
            // has no pending operations. Ownership moves exclusively to mio.
            pipe: unsafe { NamedPipe::from_raw_handle(handle.into_raw_handle()) },
            readable: false,
        }
    }

    pub(super) fn register(&mut self, poll: &Poll, token: Token) -> io::Result<()> {
        poll.registry()
            .register(&mut self.pipe, token, Interest::READABLE)?;

        // Mio can retain unread bytes across deregistration without posting
        // another readable event. Try a read before parking the loop again.
        self.readable = true;

        Ok(())
    }

    pub(super) fn deregister(&mut self, poll: &Poll) -> io::Result<()> {
        poll.registry().deregister(&mut self.pipe)?;

        self.readable = false;

        Ok(())
    }

    pub(super) fn is_ready(&self) -> bool {
        self.readable
    }
}

impl io::Read for ConoutPipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        match self.pipe.read(buf) {
            Ok(0) => {
                self.readable = false;

                // Mio reports both hangup and a zero-length peer write as zero
                // bytes. Check that the peer is still connected so an empty
                // write cannot close the terminal, while hangup stays an error.
                // SAFETY: The pipe owns a live handle; all optional output
                // pointers are null because only connection status is needed.
                let connected = unsafe {
                    PeekNamedPipe(
                        self.pipe.as_raw_handle(),
                        ptr::null_mut(),
                        0,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                };

                if connected == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(0)
                }
            }
            Ok(read) => {
                self.readable = true;

                Ok(read)
            }
            Err(error) => {
                self.readable = false;

                if error.kind() == io::ErrorKind::WouldBlock {
                    Ok(0)
                } else {
                    Err(error)
                }
            }
        }
    }
}

/// The console input stream. `write` accepts bytes only while no native write
/// is running and reports `WouldBlock` otherwise; the ready flag is set while
/// it would accept.
pub struct ConinPipe {
    // Declared before `buf`: dropping it completes the write that reads `buf`.
    end: PipeEnd,

    /// The bytes of the accepted write, kept until all of them are sent.
    buf: Vec<u8>,

    sent: usize,
}

impl ConinPipe {
    fn new(handle: OwnedHandle) -> io::Result<Self> {
        let end = PipeEnd::new(handle)?;

        end.soft.set_ready();

        Ok(Self {
            end,
            buf: Vec::new(),
            sent: 0,
        })
    }

    /// The soft-ready handle, so the `Pty` can inject the loop `Waker` at
    /// `register()` time and query writability in `drain_ready()`.
    pub fn soft(&self) -> &SoftReady {
        &self.end.soft
    }

    fn submit(&mut self) -> io::Result<()> {
        let rest = &self.buf[self.sent..];

        let started = unsafe {
            WriteFile(
                self.end.handle.as_raw_handle(),
                rest.as_ptr(),
                rest.len() as u32,
                ptr::null_mut(),
                &mut *self.end.overlapped,
            )
        };

        self.end.started(started)
    }

    /// Account for a finished native write, sending whatever it left over.
    fn settle(&mut self) -> io::Result<()> {
        while self.end.in_flight {
            let Some(transferred) = self.end.finished()? else {
                return Ok(());
            };

            self.sent += transferred;

            if self.sent < self.buf.len() {
                if transferred == 0 {
                    return Err(io::ErrorKind::WriteZero.into());
                }

                self.submit()?;
            }
        }

        Ok(())
    }
}

impl io::Write for ConinPipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.settle()?;

        if self.end.in_flight {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        // A zero-length write on a byte pipe completes the peer's read with no
        // data, which a reader can take for the end of the stream.
        if buf.is_empty() {
            return Ok(0);
        }

        let accepted = buf.len().min(WRITE_CHUNK);

        self.buf.clear();

        self.buf.extend_from_slice(&buf[..accepted]);

        self.sent = 0;

        self.submit()?;

        // A write that fit the pipe buffer is already complete; settling now
        // lets a `flush` that follows succeed without waiting for a wakeup.
        self.settle()?;

        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.settle()?;

        if self.end.in_flight {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(())
        }
    }
}
