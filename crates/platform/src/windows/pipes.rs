#[cfg(test)]
#[path = "pipes_tests.rs"]
mod pipes_tests;

use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::{JoinHandle, sleep, spawn};
use std::time::Duration;

use miow::pipe::{AnonRead, AnonWrite};
use parking_lot::{Condvar, Mutex};
use windows_sys::Win32::System::IO::CancelSynchronousIo;

use crate::windows::readiness::SoftReady;
use crate::windows::spsc::*;

struct PipeState {
    soft: SoftReady,
    done: AtomicBool,
    buffer_changed: Condvar,
    wait_tag: Mutex<()>,
}

struct PipeWorker {
    thread: Option<JoinHandle<()>>,
    state: Arc<PipeState>,
    errors: Receiver<String>,
}

impl PipeWorker {
    fn new(pump: impl FnOnce(Arc<PipeState>, Sender<String>) + Send + 'static) -> Self {
        let state = Arc::new(PipeState {
            soft: SoftReady::new(),
            done: AtomicBool::new(false),
            buffer_changed: Condvar::new(),
            wait_tag: Mutex::new(()),
        });

        let (sender, errors) = channel();
        let worker_state = state.clone();
        let thread = spawn(move || pump(worker_state, sender));

        Self {
            thread: Some(thread),
            state,
            errors,
        }
    }

    fn check_error(&mut self) -> io::Result<()> {
        if self.thread.is_none() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }

        match self.errors.try_recv() {
            Ok(error) => {
                if let Some(thread) = self.thread.take() {
                    let _ = thread.join();
                }

                Err(io::Error::new(io::ErrorKind::BrokenPipe, error))
            }

            Err(TryRecvError::Disconnected) => Err(io::ErrorKind::BrokenPipe.into()),
            Err(TryRecvError::Empty) => Ok(()),
        }
    }
}

impl Drop for PipeWorker {
    fn drop(&mut self) {
        self.state.done.store(true, Ordering::SeqCst);

        // Pair with the worker's predicate check so the stop notification
        // cannot be lost between checking the flag and parking.
        drop(self.state.wait_tag.lock());
        self.state.buffer_changed.notify_one();

        if let Some(thread) = self.thread.take() {
            while !thread.is_finished() {
                // A worker may enter native I/O after checking `done` but after
                // our first cancellation too. Retry until it observes the stop
                // flag or the pending synchronous call is cancelled.
                unsafe { CancelSynchronousIo(thread.as_raw_handle()) };

                sleep(Duration::from_millis(1));
            }

            let _ = thread.join();
        }
    }
}

/// Wraps an AnonRead pipe so that it can be read asynchronously using mio.
///
/// This is achieved by spawning a worker thread which continuously attempts
/// to read from the pipe into a buffer, which reads from the EventedAnonRead
/// object will be directed to.
///
/// This should only be considered if your application architecture requires
/// a synchronous anonymous pipe; an asynchronous NamedPipe will likely be
/// more performant.
pub struct EventedAnonRead {
    worker: PipeWorker,
    consumer: SpscBufferReader,
}

// Helper to send an error string from the worker threads
macro_rules! try_or_send {
    ($e:expr, $sender:ident) => {
        match $e {
            Ok(value) => value,
            Err(e) => {
                let _ = $sender.send(e.to_string());
                return;
            }
        }
    };
}

impl EventedAnonRead {
    pub fn new(pipe: AnonRead) -> Self {
        let (producer, consumer) = spsc_buffer(65536);

        let worker = PipeWorker::new(move |state, errors| {
            pump_pipe_to_buffer(pipe, producer, state, errors)
        });

        Self { worker, consumer }
    }

    /// The soft-ready handle, so the `Pty` can inject the loop `Waker` at
    /// `register()` time and query readiness in `drain_ready()`.
    pub fn soft(&self) -> &SoftReady {
        &self.worker.state.soft
    }
}

fn pump_pipe_to_buffer(
    mut pipe: AnonRead,
    mut producer: SpscBufferWriter,
    inner: Arc<PipeState>,
    error_sender: Sender<String>,
) {
    use std::io::Read;

    let mut tmp_buf = [0u8; 65535];

    loop {
        if inner.done.load(Ordering::SeqCst) {
            return;
        }

        // Read into temp buffer
        let nbytes = try_or_send!(pipe.read(&mut tmp_buf[..]), error_sender);

        // Write from the temp buffer into the producer
        let mut written = 0usize;

        while written < nbytes {
            // Wait for buffer to clear if need be. The predicate is
            // re-checked under the lock and notifiers acquire this
            // lock after changing buffer state, so a drain+notify
            // cannot slip between the check and the wait (a lost
            // wakeup here strands bytes until the next notify).
            if producer.is_full() {
                let mut wait_tag = inner.wait_tag.lock();

                while producer.is_full() && !inner.done.load(Ordering::SeqCst) {
                    inner.buffer_changed.wait(&mut wait_tag);
                }

                if inner.done.load(Ordering::SeqCst) {
                    return;
                }
            }

            written += producer.write_from_slice(&tmp_buf[written..nbytes]);

            if !inner.soft.is_ready() {
                inner.soft.set_ready();
            }
        }
    }
}

impl io::Read for EventedAnonRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.worker.check_error()?;

        let nbytes = self.consumer.read_to_slice(buf);

        if self.consumer.is_empty() {
            // Level-like: clear only when the buffer is fully drained.
            self.worker.state.soft.clear();

            // Possible race: the consumer may think the queue is empty but by the time
            // the flag is cleared the producer thread may have written data. We avoid
            // the race by re-checking and re-arming if necessary.
            if !self.consumer.is_empty() {
                self.worker.state.soft.set_ready();
            }
        }

        // Pairs with the worker's under-lock predicate check: taking (and
        // releasing) the wait lock after draining guarantees the worker is
        // either before its check (and will see the space) or already parked
        // (and will get this notify).
        drop(self.worker.state.wait_tag.lock());
        self.worker.state.buffer_changed.notify_one();

        Ok(nbytes)
    }
}

/// Wraps an AnonWrite pipe so that it can be written asynchronously using mio.
///
/// This is achieved by spawning a worker thread which continuously attempts
/// to write to the pipe from a buffer, which writes to the EventedAnonWrite
/// object will be directed to.
///
/// This should only be considered if your application architecture requires
/// a synchronous anonymous pipe; an asynchronous NamedPipe will likely be
/// more performant.
pub struct EventedAnonWrite {
    worker: PipeWorker,
    producer: SpscBufferWriter,
}

impl EventedAnonWrite {
    pub fn new(pipe: AnonWrite) -> Self {
        let (producer, consumer) = spsc_buffer(65536);

        let worker = PipeWorker::new(move |state, errors| {
            pump_buffer_to_pipe(pipe, consumer, state, errors)
        });

        Self { worker, producer }
    }

    /// The soft-ready handle, so the `Pty` can inject the loop `Waker` at
    /// `register()` time and query writability in `drain_ready()`.
    pub fn soft(&self) -> &SoftReady {
        &self.worker.state.soft
    }
}

fn pump_buffer_to_pipe(
    mut pipe: AnonWrite,
    mut consumer: SpscBufferReader,
    inner: Arc<PipeState>,
    error_sender: Sender<String>,
) {
    use std::io::Write;

    let mut tmp_buf = [0u8; 65535];

    // The buffer starts empty, so the loop may write immediately.
    inner.soft.set_ready();

    loop {
        if inner.done.load(Ordering::SeqCst) {
            return;
        }

        // Read into temp buffer while holding the lock
        let nbytes = {
            // Wait for buffer to have contents. The predicate is
            // re-checked under the lock and notifiers acquire this
            // lock after changing buffer state, so a write+notify
            // from the app thread cannot slip between the check and
            // the wait — a lost wakeup here would leave the written
            // bytes sitting in the ring until the next write call.
            if consumer.is_empty() {
                let mut wait_tag = inner.wait_tag.lock();

                while consumer.is_empty() && !inner.done.load(Ordering::SeqCst) {
                    inner.buffer_changed.wait(&mut wait_tag);
                }

                if inner.done.load(Ordering::SeqCst) {
                    return;
                }
            }

            let nbytes = consumer.read_to_slice(&mut tmp_buf);

            // Buffer has space again → the loop may write more.
            if !inner.soft.is_ready() {
                inner.soft.set_ready();
            }

            nbytes
        };

        let mut written = 0usize;

        while written < nbytes {
            written += try_or_send!(pipe.write(&tmp_buf[written..nbytes]), error_sender);
        }
    }
}

impl io::Write for EventedAnonWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.worker.check_error()?;

        let nbytes = self.producer.write_from_slice(buf);

        if self.producer.is_full() {
            // Backpressure: buffer full → not writable until the worker drains it.
            self.worker.state.soft.clear();

            // Possible race: the producer may think the buffer is full but by the time
            // the flag is cleared the consumer thread may have read data. Re-check and
            // re-arm to work around this.
            if !self.producer.is_full() {
                self.worker.state.soft.set_ready();
            }
        }

        // Pairs with the worker's under-lock predicate check: taking (and
        // releasing) the wait lock after publishing the bytes guarantees the
        // worker is either before its check (and will see the data) or already
        // parked (and will get this notify).
        drop(self.worker.state.wait_tag.lock());
        self.worker.state.buffer_changed.notify_one();

        Ok(nbytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
