//! Overlapped ConPTY pipes.
//!
//! `CreatePipe` cannot open an end for overlapped I/O, which forces a blocked
//! thread per direction. Each pair here is a single-instance named pipe
//! instead: the end this process keeps is overlapped, and the end handed to
//! the console host stays synchronous, which is what the host expects.
//!
//! Output reads use Tokio's named pipe, so completions arrive directly at the
//! runtime's IOCP. Input writes complete against an auto-reset event; its wait
//! callback wakes the waiting task. Writes can also settle on demand without
//! a runtime.

#[cfg(test)]
#[path = "pipes_tests.rs"]
mod pipes_tests;

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU32, Ordering};
use std::task::{Context, Poll as TaskPoll, ready};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{io, mem, process, ptr};

use futures::task::AtomicWaker;
use tokio::net::windows::named_pipe::NamedPipeServer;
use windows_sys::Win32::Foundation::{
    ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PeekNamedPipe,
};
use windows_sys::Win32::System::Threading::{CreateEventW, WT_EXECUTEINWAITTHREAD};

use crate::windows::registered_wait::RegisteredWait;
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

/// Which way bytes flow through a pipe, seen from this process.
#[derive(Clone, Copy)]
pub(crate) enum Direction {
    /// The peer writes and this process reads.
    Inbound,
    /// This process writes and the peer reads.
    Outbound,
}

/// Create the pipe carrying console output: the overlapped end read here, and
/// the synchronous write end for the console host. The reader is registered
/// with the shared runtime's IOCP here, so creation can stay synchronous while
/// the first read needs no runtime context of its own.
pub(crate) fn conout_pair() -> io::Result<(ConoutPipe, OwnedHandle)> {
    let (ours, theirs) = pipe_pair(Direction::Inbound)?;

    let _runtime = nmt_runtime::handle().enter();

    // SAFETY: The connected handle has no pending I/O, and ownership moves
    // exclusively to Tokio.
    let pipe = unsafe { NamedPipeServer::from_raw_handle(ours.into_raw_handle()) }?;

    Ok((ConoutPipe { pipe }, theirs))
}

/// Create the pipe carrying console input: the synchronous read end for the
/// console host, and the overlapped end written here.
pub(crate) fn conin_pair() -> io::Result<(OwnedHandle, ConinPipe)> {
    let (ours, theirs) = pipe_pair(Direction::Outbound)?;

    Ok((theirs, ConinPipe::new(ours)?))
}

/// Pipes for one of a child's standard streams. Anonymous pipes cannot be
/// overlapped, so Tokio's own child stdio parks a blocking-pool thread on
/// every pending read. Here the parent end is registered with the runtime's
/// IOCP instead, and the child's end stays synchronous as ordinary console
/// programs expect. Call inside a runtime context.
pub(crate) fn child_stdio_pair(direction: Direction) -> io::Result<(NamedPipeServer, OwnedHandle)> {
    let (ours, theirs) = pipe_pair(direction)?;

    // SAFETY: The connected handle has no pending I/O, and ownership moves
    // exclusively to Tokio.
    let ours = unsafe { NamedPipeServer::from_raw_handle(ours.into_raw_handle()) }?;

    Ok((ours, theirs))
}

/// One connected pipe: the overlapped end kept here, then the synchronous end
/// opened for the peer.
///
/// The name only has to be unique on this machine. `FIRST_PIPE_INSTANCE` makes
/// creation fail if another process already owns the name, and the single
/// instance is taken by the open below, so the pipe can never gain a second
/// peer: if another process connects first, the open fails and no terminal
/// data is ever written to it.
fn pipe_pair(direction: Direction) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (access, peer_access) = match direction {
        Direction::Inbound => (PIPE_ACCESS_INBOUND, GENERIC_WRITE),
        Direction::Outbound => (PIPE_ACCESS_OUTBOUND, GENERIC_READ),
    };

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
/// Waking consumes the registered waker, so a burst of completions before the
/// task re-registers posts one wakeup.
unsafe extern "system" fn operation_completed(ctx: *mut c_void, _timed_out: bool) {
    // Borrow only: the registration owns the waker and frees it after a
    // blocking unregister has excluded any callback still running.
    let waker = unsafe { &*(ctx as *const AtomicWaker) };

    waker.wake();
}

/// The overlapped input end and the single write it runs at a time.
struct PipeEnd {
    handle: OwnedHandle,

    /// Boxed so the address the kernel holds stays valid when this moves.
    overlapped: Box<OVERLAPPED>,

    /// Auto-reset, so the registered wait fires once per completion. Held so
    /// the handle inside `overlapped` stays open for as long as it is used.
    _event: OwnedHandle,

    wait: RegisteredWait<AtomicWaker>,

    in_flight: bool,
}

// The raw pointer inside `overlapped` refers to an event this value owns.
unsafe impl Send for PipeEnd {}

impl PipeEnd {
    fn new(handle: OwnedHandle) -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };

        if event.is_null() {
            return Err(io::Error::last_os_error());
        }

        let event = unsafe { OwnedHandle::from_raw_handle(event) };

        let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { mem::zeroed() });

        overlapped.hEvent = event.as_raw_handle();

        let wait = RegisteredWait::new(
            event.as_raw_handle(),
            AtomicWaker::new(),
            operation_completed,
            WT_EXECUTEINWAITTHREAD,
        )?;

        Ok(Self {
            handle,
            overlapped,
            _event: event,
            wait,
            in_flight: false,
        })
    }

    /// Install before checking completion so a concurrent callback cannot be
    /// lost between the check and the task suspending.
    fn register_task_waker(&self, cx: &Context<'_>) {
        self.wait.context().register(cx.waker());
    }

    /// Start one native write of `bytes`. A write that finished inside the
    /// call still reports its byte count through `result`.
    fn submit(&mut self, bytes: &[u8]) -> io::Result<()> {
        let succeeded = unsafe {
            WriteFile(
                self.handle.as_raw_handle(),
                bytes.as_ptr(),
                bytes.len() as u32,
                ptr::null_mut(),
                &mut *self.overlapped,
            )
        };

        if succeeded == 0 {
            let error = io::Error::last_os_error();

            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
        }

        self.in_flight = true;

        Ok(())
    }

    /// The byte count of the running operation, or `Pending` while it runs.
    fn result(&mut self) -> TaskPoll<io::Result<usize>> {
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

            return TaskPoll::Ready(Ok(transferred as usize));
        }

        let error = io::Error::last_os_error();

        if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
            return TaskPoll::Pending;
        }

        self.in_flight = false;

        TaskPoll::Ready(Err(error))
    }
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        // The callback and the wait below would both consume the auto-reset
        // event, and only one of them wakes. Stopping the callback first
        // leaves a later completion's signal to the wait alone.
        self.wait.unregister();

        if !self.in_flight {
            return;
        }

        unsafe {
            // The kernel still references `overlapped` and the owner's buffer.
            // Both are freed after this returns, so the cancelled operation has
            // to be seen through to its completion first.
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

/// The console output stream, driven by the shared runtime's IOCP. Reads
/// report `BrokenPipe` after hangup.
pub struct ConoutPipe {
    // Mio 1.2.3, which backs Tokio's named pipes, uses a 4 KiB internal read
    // buffer with no public size setting. PIPE_BUFFER only sizes the kernel
    // buffer; larger caller read slices do not enlarge mio's buffer. Direct
    // IOCP removes the wait-thread hop, but the smaller reads increase
    // completion and poll frequency during sustained output and can reduce
    // throughput. Using 64 KiB internal reads requires a mio dependency patch;
    // its effect on throughput and short-message latency needs measurement.
    pipe: NamedPipeServer,
}

impl ConoutPipe {
    pub(super) fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> TaskPoll<io::Result<usize>> {
        loop {
            ready!(self.pipe.poll_read_ready(cx))?;

            match self.pipe.try_read(buf) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Ok(0) if !buf.is_empty() => {
                    // An empty peer write also produces a zero-byte completion.
                    // Keep waiting while connected, instead of closing the tab.
                    // SAFETY: The pipe owns the handle, and optional output
                    // pointers are null because only connection status is used.
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
                        return TaskPoll::Ready(Err(io::Error::last_os_error()));
                    }
                }
                result => return TaskPoll::Ready(result),
            }
        }
    }
}

/// The console input stream. `poll_write` accepts bytes only while no native
/// write is running and stays pending otherwise.
pub struct ConinPipe {
    // Declared before `buf`: dropping it completes the write that reads `buf`.
    end: PipeEnd,

    /// The bytes of the accepted write, kept until all of them are sent.
    buf: Vec<u8>,

    sent: usize,
}

impl ConinPipe {
    fn new(handle: OwnedHandle) -> io::Result<Self> {
        Ok(Self {
            end: PipeEnd::new(handle)?,
            buf: Vec::new(),
            sent: 0,
        })
    }

    pub(super) fn poll_write(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> TaskPoll<io::Result<usize>> {
        self.end.register_task_waker(cx);

        ready!(self.settle())?;

        // A zero-length write on a byte pipe completes the peer's read with no
        // data, which a reader can take for the end of the stream.
        if buf.is_empty() {
            return TaskPoll::Ready(Ok(0));
        }

        let accepted = buf.len().min(WRITE_CHUNK);

        self.buf.clear();

        self.buf.extend_from_slice(&buf[..accepted]);

        self.sent = 0;

        self.end.submit(&self.buf)?;

        // A write that fit the pipe buffer is already complete; settling now
        // lets a flush that follows succeed without waiting for a wakeup.
        if let TaskPoll::Ready(Err(error)) = self.settle() {
            return TaskPoll::Ready(Err(error));
        }

        TaskPoll::Ready(Ok(accepted))
    }

    /// Complete once every accepted byte reached the pipe.
    pub(super) fn poll_flush(&mut self, cx: &mut Context<'_>) -> TaskPoll<io::Result<()>> {
        self.end.register_task_waker(cx);

        self.settle()
    }

    /// Account for finished native writes, sending whatever they left over.
    /// `Pending` while a write is still running.
    fn settle(&mut self) -> TaskPoll<io::Result<()>> {
        while self.end.in_flight {
            let transferred = ready!(self.end.result())?;

            self.sent += transferred;

            if self.sent < self.buf.len() {
                if transferred == 0 {
                    return TaskPoll::Ready(Err(io::ErrorKind::WriteZero.into()));
                }

                self.end.submit(&self.buf[self.sent..])?;
            }
        }

        TaskPoll::Ready(Ok(()))
    }
}
