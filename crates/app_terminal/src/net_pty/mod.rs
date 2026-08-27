//! `EventedPty` backed by a remote session instead of a local ConPTY, so a
//! remote terminal tab reuses the entire `PtyPipe` → engine → render → wake
//! pipeline unchanged. The only difference from a local session is where bytes
//! come from and go to:
//!
//! - reader: the attach snapshot's VT state first, then live `Output` bytes.
//! - writer: terminal-encoded input, forwarded as `Frame::Input`.
//! - resize: `Frame::Resize`.
//! - child exit: the remote session ending.
//!
//! Readiness mirrors the ConPTY model (`nmt_platform::SoftReady`): a background
//! drain thread pushes network bytes into a shared buffer and wakes the event
//! loop, because there is no real OS readiness source for the network stream.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use nmt_platform::{
    ChildEvent, EventedPty, Interest, Poll, ProcessReadWrite, SoftReady, Token, Waker,
    WinsizeBuilder,
};
use nmt_remote_net::{RemoteInput, RemoteSession, SessionByteEvent};
use parking_lot::Mutex;
use tracing::warn;

/// Cap on unread network bytes. A local ConPTY throttles its child when the
/// reader falls behind; the network stream has no such brake, so a remote
/// command dumping megabytes at a stalled engine would grow this queue without
/// bound. Overflow drops the oldest bytes — the newest output is what the
/// screen shows, and a client this far behind has already lost the exact
/// scrollback.
const MAX_BUFFERED_BYTES: usize = 8 * 1024 * 1024;

pub struct NetPty {
    reader: NetReader,
    writer: NetWriter,
    input: RemoteInput,
    read_ready: SoftReady,
    child_ready: SoftReady,
    exited: Arc<AtomicBool>,
    read_token: Token,
    write_token: Token,
    child_token: Token,
}

impl NetPty {
    /// Build a PTY from an attached remote session, spawning the drain thread
    /// that feeds the snapshot then live output into the read buffer.
    pub fn new(session: RemoteSession) -> Self {
        let snapshot = session.snapshot().vt.clone();
        let input = session.input();
        let output = session.into_output();

        let buffer = Arc::new(Mutex::new(VecDeque::<u8>::from(snapshot)));
        let read_ready = SoftReady::new();
        let child_ready = SoftReady::new();
        let exited = Arc::new(AtomicBool::new(false));

        // Snapshot bytes are already buffered, so the first poll must read them.
        read_ready.set_ready();

        let drain_buffer = Arc::clone(&buffer);
        let drain_read_ready = read_ready.clone();
        let drain_child_ready = child_ready.clone();
        let drain_exited = Arc::clone(&exited);
        thread::Builder::new()
            .name("net-pty-drain".into())
            .spawn(move || {
                let mut overflowed = false;
                while let Ok(event) = output.recv() {
                    match event {
                        SessionByteEvent::Output(bytes) => {
                            let dropped = push_bounded(&mut drain_buffer.lock(), bytes);
                            if dropped && !overflowed {
                                overflowed = true;
                                warn!(
                                    "remote output outran the terminal engine; \
                                     dropping the oldest buffered bytes"
                                );
                            }
                            drain_read_ready.set_ready();
                        }
                        SessionByteEvent::Exited => break,
                    }
                }
                // The channel closing (host gone / session ended) is a child exit.
                drain_exited.store(true, Ordering::SeqCst);
                drain_child_ready.set_ready();
            })
            .expect("spawn net-pty drain thread");

        Self {
            reader: NetReader {
                buffer,
                read_ready: read_ready.clone(),
            },
            writer: NetWriter {
                input: input.clone(),
            },
            input,
            read_ready,
            child_ready,
            exited,
            read_token: Token(0),
            write_token: Token(0),
            child_token: Token(0),
        }
    }
}

/// Append `bytes`, discarding the oldest data once the queue exceeds
/// [`MAX_BUFFERED_BYTES`]. Reports whether anything had to be discarded.
fn push_bounded(queue: &mut VecDeque<u8>, bytes: Vec<u8>) -> bool {
    queue.extend(bytes);
    let Some(excess) = queue.len().checked_sub(MAX_BUFFERED_BYTES) else {
        return false;
    };
    queue.drain(..excess);
    excess > 0
}

pub struct NetReader {
    buffer: Arc<Mutex<VecDeque<u8>>>,
    read_ready: SoftReady,
}

impl Read for NetReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut queue = self.buffer.lock();
        if queue.is_empty() {
            // Drained: clear the level flag so the loop can block until the
            // drain thread signals more data. `Ok(0)` means "no more readable",
            // matching how the event loop treats a caught-up PTY.
            self.read_ready.clear();
            return Ok(0);
        }
        let n = queue.len().min(buf.len());
        for slot in buf.iter_mut().take(n) {
            *slot = queue.pop_front().expect("len checked");
        }
        if queue.is_empty() {
            self.read_ready.clear();
        }
        Ok(n)
    }
}

pub struct NetWriter {
    input: RemoteInput,
}

impl Write for NetWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // The network sink never blocks; the send is fire-and-forget (a dropped
        // channel just means the session is tearing down).
        self.input.send_input(buf.to_vec());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ProcessReadWrite for NetPty {
    type Reader = NetReader;
    type Writer = NetWriter;

    fn register(
        &mut self,
        _poll: &Poll,
        token: &mut dyn Iterator<Item = Token>,
        _interest: Interest,
        waker: &Arc<Waker>,
    ) -> io::Result<()> {
        self.read_token = token.next().expect("read token");
        self.write_token = token.next().expect("write token");
        self.child_token = token.next().expect("child token");
        // The drain thread signals the loop through the same waker ConPTY uses.
        self.read_ready.set_waker(waker.clone());
        self.child_ready.set_waker(waker.clone());
        Ok(())
    }

    fn reregister(&mut self, _poll: &Poll, _interest: Interest) -> io::Result<()> {
        Ok(())
    }

    fn deregister(&mut self, _poll: &Poll) -> io::Result<()> {
        Ok(())
    }

    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.reader
    }

    fn read_token(&self) -> Token {
        self.read_token
    }

    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.writer
    }

    fn write_token(&self) -> Token {
        self.write_token
    }

    fn drain_ready(&self) -> Vec<Token> {
        let mut ready = Vec::with_capacity(3);
        if self.read_ready.is_ready() {
            ready.push(self.read_token);
        }
        // Writability is always signalled: the network sink never blocks, so
        // queued input is flushed on the next wakeup. Excluded from
        // `has_ready` below so it never forces a busy-spin.
        ready.push(self.write_token);
        if self.child_ready.is_ready() {
            ready.push(self.child_token);
        }
        ready
    }

    fn has_ready(&self) -> bool {
        // Only unconsumed work (buffered read data, pending exit) may force a
        // zero-timeout poll; the always-ready write token is deliberately left
        // out to avoid a 100% CPU spin.
        self.read_ready.is_ready() || self.child_ready.is_ready()
    }

    fn set_winsize(&mut self, size: WinsizeBuilder) -> io::Result<()> {
        self.input.send_resize(size.cols, size.rows);
        Ok(())
    }
}

impl EventedPty for NetPty {
    fn child_event_token(&self) -> Token {
        self.child_token
    }

    fn next_child_event(&mut self) -> Option<ChildEvent> {
        // Report the exit exactly once; the event loop breaks on the first.
        if self.exited.swap(false, Ordering::SeqCst) {
            Some(ChildEvent::Exited)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests;
