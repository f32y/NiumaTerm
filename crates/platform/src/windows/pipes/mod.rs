use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::{JoinHandle, spawn};

use miow::pipe::{AnonRead, AnonWrite};
use parking_lot::{Condvar, Mutex};
use windows_sys::Win32::System::IO::CancelSynchronousIo;

use crate::windows::readiness::SoftReady;
use crate::windows::spsc::*;

struct WaitTag {}

struct EventedAnonReadInner {
    soft: SoftReady,
    done: AtomicBool,
    sig_buffer_not_full: Condvar,
    wait_tag: Mutex<WaitTag>,
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
    // Is an Option so it can be moved out and joined in the Drop impl.
    thread: Option<JoinHandle<()>>,
    consumer: SpscBufferReader,
    inner: Arc<EventedAnonReadInner>,
    error_receiver: Receiver<String>,
}

// Helper to send an error string from the worker threads
macro_rules! try_or_send {
    ($e:expr, $sender:ident) => {
        match $e {
            Ok(value) => value,
            Err(e) => {
                $sender
                    .send(format!("{}", e))
                    .expect("Could not send error");
                return;
            }
        }
    };
}

impl EventedAnonRead {
    pub fn new(mut pipe: AnonRead) -> Self {
        let (mut producer, consumer) = spsc_buffer(65536);

        let done = AtomicBool::new(false);

        let sig_buffer_not_full = Condvar::new();
        let wait_tag = Mutex::new(WaitTag {});

        let (error_sender, error_receiver) = channel();

        let inner = Arc::new(EventedAnonReadInner {
            soft: SoftReady::new(),
            done,
            sig_buffer_not_full,
            wait_tag,
        });

        let thread = {
            let inner = inner.clone();

            spawn(move || {
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
                                inner.sig_buffer_not_full.wait(&mut wait_tag);
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
            })
        };

        Self {
            thread: Some(thread),
            consumer,
            inner,
            error_receiver,
        }
    }
}

impl io::Read for EventedAnonRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.thread.is_none() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, ""));
        }

        match self.error_receiver.try_recv() {
            Ok(err) => {
                // Other thread will be closing; the pipe error is already in
                // hand, so a worker panic on top of it is not worth
                // propagating into the caller.
                let _ = self.thread.take().unwrap().join();

                return Err(io::Error::new(io::ErrorKind::BrokenPipe, err));
            }
            Err(TryRecvError::Disconnected) => {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, ""));
            }
            Err(TryRecvError::Empty) => {}
        }

        let nbytes = self.consumer.read_to_slice(buf);

        if self.consumer.is_empty() {
            // Level-like: clear only when the buffer is fully drained.
            self.inner.soft.clear();

            // Possible race: the consumer may think the queue is empty but by the time
            // the flag is cleared the producer thread may have written data. We avoid
            // the race by re-checking and re-arming if necessary.
            if !self.consumer.is_empty() {
                self.inner.soft.set_ready();
            }
        }

        // Pairs with the worker's under-lock predicate check: taking (and
        // releasing) the wait lock after draining guarantees the worker is
        // either before its check (and will see the space) or already parked
        // (and will get this notify).
        drop(self.inner.wait_tag.lock());
        self.inner.sig_buffer_not_full.notify_one();
        Ok(nbytes)
    }
}

impl EventedAnonRead {
    /// The soft-ready handle, so the `Pty` can inject the loop `Waker` at
    /// `register()` time and query readiness in `drain_ready()`.
    pub fn soft(&self) -> &SoftReady {
        &self.inner.soft
    }
}

impl Drop for EventedAnonRead {
    fn drop(&mut self) {
        self.inner.done.store(true, Ordering::SeqCst);

        // Lock/unlock before notifying so the done flag cannot be missed by a
        // worker between its predicate check and its wait.
        drop(self.inner.wait_tag.lock());
        self.inner.sig_buffer_not_full.notify_one();

        // The thread may already have been taken and joined by the read()
        // error path; nothing left to do then.
        if let Some(thread) = self.thread.take() {
            // Stop reader thread waiting for pipe contents
            unsafe {
                CancelSynchronousIo(thread.as_raw_handle());
            }

            // A panicking worker must not turn Drop into a panic (which
            // aborts when already unwinding); the thread is gone either way.
            let _ = thread.join();
        }
    }
}

struct EventedAnonWriteInner {
    soft: SoftReady,
    done: AtomicBool,
    sig_buffer_not_empty: Condvar,
    wait_tag: Mutex<WaitTag>,
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
    // Is an Option so it can be moved out and joined in the Drop impl
    thread: Option<JoinHandle<()>>,
    producer: SpscBufferWriter,
    inner: Arc<EventedAnonWriteInner>,
    error_receiver: Receiver<String>,
}

impl EventedAnonWrite {
    pub fn new(mut pipe: AnonWrite) -> Self {
        let (producer, mut consumer) = spsc_buffer(65536);

        let done = AtomicBool::new(false);

        let sig_buffer_not_empty = Condvar::new();
        let wait_tag = Mutex::new(WaitTag {});

        let inner = Arc::new(EventedAnonWriteInner {
            soft: SoftReady::new(),
            done,
            sig_buffer_not_empty,
            wait_tag,
        });

        let (error_sender, error_receiver) = channel();

        let thread = {
            let inner = inner.clone();

            spawn(move || {
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
                                inner.sig_buffer_not_empty.wait(&mut wait_tag);
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
                        written +=
                            try_or_send!(pipe.write(&tmp_buf[written..nbytes]), error_sender);
                    }
                }
            })
        };

        Self {
            thread: Some(thread),
            producer,
            inner,
            error_receiver,
        }
    }
}

impl io::Write for EventedAnonWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.thread.is_none() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, ""));
        }

        match self.error_receiver.try_recv() {
            Ok(err) => {
                // Other thread will be closing; the pipe error is already in
                // hand, so a worker panic on top of it is not worth
                // propagating into the caller.
                let _ = self.thread.take().unwrap().join();

                return Err(io::Error::new(io::ErrorKind::BrokenPipe, err));
            }
            Err(TryRecvError::Disconnected) => {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, ""));
            }
            Err(TryRecvError::Empty) => {}
        }

        let nbytes = self.producer.write_from_slice(buf);
        if self.producer.is_full() {
            // Backpressure: buffer full → not writable until the worker drains it.
            self.inner.soft.clear();

            // Possible race: the producer may think the buffer is full but by the time
            // the flag is cleared the consumer thread may have read data. Re-check and
            // re-arm to work around this.
            if !self.producer.is_full() {
                self.inner.soft.set_ready();
            }
        }

        // Pairs with the worker's under-lock predicate check: taking (and
        // releasing) the wait lock after publishing the bytes guarantees the
        // worker is either before its check (and will see the data) or already
        // parked (and will get this notify).
        drop(self.inner.wait_tag.lock());
        self.inner.sig_buffer_not_empty.notify_one();

        Ok(nbytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl EventedAnonWrite {
    /// The soft-ready handle, so the `Pty` can inject the loop `Waker` at
    /// `register()` time and query writability in `drain_ready()`.
    pub fn soft(&self) -> &SoftReady {
        &self.inner.soft
    }
}

impl Drop for EventedAnonWrite {
    fn drop(&mut self) {
        self.inner.done.store(true, Ordering::SeqCst);

        // Stop the writer thread waiting for contents. Lock/unlock before
        // notifying so the done flag cannot be missed by a worker between its
        // predicate check and its wait.
        drop(self.inner.wait_tag.lock());
        self.inner.sig_buffer_not_empty.notify_one();

        // A panicking worker must not turn Drop into a panic (which aborts
        // when already unwinding); the thread is gone either way.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
