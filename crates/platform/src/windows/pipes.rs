//! Overlapped ConPTY pipes.
//!
//! `CreatePipe` cannot open an end for overlapped I/O, which forces a blocked
//! thread per direction. Each pair here is a single-instance named pipe
//! instead: the end this process keeps is overlapped, and the end handed to
//! the console host stays synchronous, which is what the host expects.
//!
//! Output reads use Tokio's named pipe, so completions arrive directly at the
//! runtime's IOCP. Input writes complete against an auto-reset event; its wait
//! callback marks [`SoftReady`] and wakes the waiting task. Writes can also
//! settle on demand without a runtime.

#[cfg(test)]
#[path = "pipes_tests.rs"]
mod pipes_tests;

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU32, Ordering};
use std::task::{Context, Poll as TaskPoll, ready};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{io, mem, process, ptr};

use tokio::net::windows::named_pipe::NamedPipeServer;
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

/// Pipes for one of a child's standard streams; `inbound` carries the child's
/// output here. Anonymous pipes cannot be overlapped, so Tokio's own child
/// stdio parks a blocking-pool thread on every pending read. Here the parent
/// end is registered with the runtime's IOCP instead, and the child's end
/// stays synchronous as ordinary console programs expect. Call inside a
/// runtime context.
pub(crate) fn child_stdio_pair(inbound: bool) -> io::Result<(NamedPipeServer, OwnedHandle)> {
    let (ours, theirs) = if inbound {
        pipe_pair(PIPE_ACCESS_INBOUND, GENERIC_WRITE)?
    } else {
        pipe_pair(PIPE_ACCESS_OUTBOUND, GENERIC_READ)?
    };

    // SAFETY: The connected handle has no pending I/O, and ownership moves
    // exclusively to Tokio.
    let ours = unsafe { NamedPipeServer::from_raw_handle(ours.into_raw_handle()) }?;

    Ok((ours, theirs))
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

/// The console output stream, driven by the Tokio runtime's IOCP after
/// [`ConoutPipe::start_async`]. Reads report `BrokenPipe` after hangup.
pub struct ConoutPipe {
    // Mio 1.2.3, which backs Tokio's named pipes, uses a 4 KiB internal read
    // buffer with no public size setting. PIPE_BUFFER only sizes the kernel
    // buffer; larger caller read slices do not enlarge mio's buffer. Direct
    // IOCP removes the wait-thread hop, but the smaller reads increase
    // completion and poll frequency during sustained output and can reduce
    // throughput. Using 64 KiB internal reads requires a mio dependency patch;
    // its effect on throughput and short-message latency needs measurement.
    pipe: ReadSource,
}

/// The IOCP association is made inside a runtime context, which session
/// creation does not have, so the handle waits until the owner task starts.
enum ReadSource {
    Unregistered(Option<OwnedHandle>),
    Registered(NamedPipeServer),
}

impl ConoutPipe {
    fn new(handle: OwnedHandle) -> Self {
        Self {
            pipe: ReadSource::Unregistered(Some(handle)),
        }
    }

    pub(super) fn start_async(&mut self) -> io::Result<()> {
        let ReadSource::Unregistered(handle) = &mut self.pipe else {
            return Err(io::Error::other("pipe is already registered"));
        };

        let handle = handle
            .take()
            .ok_or_else(|| io::Error::other("pipe initialization failed"))?;

        // SAFETY: No I/O was submitted before this sole ownership transfer.
        // IOCP association cannot be moved from a different live poller.
        let pipe = unsafe { NamedPipeServer::from_raw_handle(handle.into_raw_handle()) }?;

        self.pipe = ReadSource::Registered(pipe);

        Ok(())
    }

    pub(super) fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> TaskPoll<io::Result<usize>> {
        let ReadSource::Registered(pipe) = &mut self.pipe else {
            return TaskPoll::Ready(Err(io::Error::other("pipe has no async registration")));
        };

        loop {
            ready!(pipe.poll_read_ready(cx))?;

            match pipe.try_read(buf) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Ok(0) if !buf.is_empty() => {
                    // An empty peer write also produces a zero-byte completion.
                    // Keep waiting while connected, instead of closing the tab.
                    // SAFETY: The pipe owns the handle, and optional output
                    // pointers are null because only connection status is used.
                    let connected = unsafe {
                        PeekNamedPipe(
                            pipe.as_raw_handle(),
                            ptr::null_mut(),
                            0,
                            ptr::null_mut(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                        )
                    };

                    if connected == 0 {
                        return TaskPoll::Ready(Err(io::Error::last_os_error()));
                    }
                }
                result => return TaskPoll::Ready(result),
            }
        }
    }
}

/// The console input stream. `poll_write` accepts bytes only while no native
/// write is running and stays pending otherwise; the ready flag is set while
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

    pub(super) fn poll_write(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> TaskPoll<io::Result<usize>> {
        // Installed before settling, so a completion racing the check still
        // wakes the task that is about to suspend.
        self.end.soft.register_task_waker(cx.waker());

        self.settle()?;

        if self.end.in_flight {
            return TaskPoll::Pending;
        }

        // A zero-length write on a byte pipe completes the peer's read with no
        // data, which a reader can take for the end of the stream.
        if buf.is_empty() {
            return TaskPoll::Ready(Ok(0));
        }

        let accepted = buf.len().min(WRITE_CHUNK);

        self.buf.clear();

        self.buf.extend_from_slice(&buf[..accepted]);

        self.sent = 0;

        self.submit()?;

        // A write that fit the pipe buffer is already complete; settling now
        // lets a flush that follows succeed without waiting for a wakeup.
        self.settle()?;

        TaskPoll::Ready(Ok(accepted))
    }

    /// Complete once every accepted byte reached the pipe.
    pub(super) fn poll_flush(&mut self, cx: &mut Context<'_>) -> TaskPoll<io::Result<()>> {
        self.end.soft.register_task_waker(cx.waker());

        self.settle()?;

        if self.end.in_flight {
            TaskPoll::Pending
        } else {
            TaskPoll::Ready(Ok(()))
        }
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
