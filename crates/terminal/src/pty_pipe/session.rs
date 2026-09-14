use std::error;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::thread::{Builder, JoinHandle};

use nmt_config::colors::Colors;
use nmt_platform::EventedPty;

use crate::ansi::CursorShape;
use crate::event::{EventListener, Msg, MsgSender};
use crate::pty_pipe::PtyPipe;
use crate::publication::FrameStore;
use crate::render_buffer::RenderBuffer;

/// Observes the exact VT bytes accepted by the engine, on the owner thread.
/// Returning before the next command preserves checkpoint and output ordering;
/// observers must not wait for work submitted to this same event loop.
pub type OutputSink = Arc<dyn Fn(Arc<[u8]>) + Send + Sync>;

/// Construction settings for [`start_session`].
pub struct SessionOptions {
    pub cols: u16,
    pub rows: u16,

    /// Event route id stamped onto every event this pipe emits (multi-tab safety).
    pub route_id: usize,

    /// Theme palette pushed into the engine so SGR-indexed and default colors
    /// resolve before the first PTY byte.
    pub colors: Colors,

    pub cursor_shape: CursorShape,

    /// Scrollback budget in lines.
    pub scrollback_lines: usize,

    /// Freeze finished commands into engine blocks; `false` is the classic
    /// single-grid fallback.
    pub engine_blocks: bool,

    /// Whether this pipe answers DA/DSR/OSC queries. Off for a headless host
    /// whose attached frontend owns terminal identity and theme.
    pub terminal_responses: bool,

    pub output_sink: Option<OutputSink>,
}

/// Shared handles to one running terminal session, returned by [`start_session`].
pub struct SessionHandles {
    pub worker: SessionWorker,

    /// Immutable viewport publications, retained independently by each reader.
    pub render_buffer: Arc<FrameStore>,

    /// VT modes published by the pipe; the input path reads them lock-free.
    pub vt_modes: Arc<AtomicU32>,

    /// Sender for input, resize, and shutdown messages to the PTY thread.
    pub messenger: MsgSender,
}

/// Keeps the PTY worker alive until its owner closes the session, then waits
/// for final frame publication and PTY cleanup before releasing shared state.
pub struct SessionWorker {
    messenger: MsgSender,
    thread: Option<JoinHandle<()>>,
}

#[cfg(test)]
impl SessionWorker {
    pub(crate) fn without_thread_for_test(messenger: MsgSender) -> Self {
        Self {
            messenger,
            thread: None,
        }
    }
}

impl Drop for SessionWorker {
    fn drop(&mut self) {
        let _ = self.messenger.send(Msg::Shutdown);

        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("PTY worker panicked");
        }
    }
}

/// Build the engine and render buffer, configure the pipe, and start the PTY
/// event-loop thread. The single construction entry point: callers receive
/// every shared handle from one call instead of assembling buffers up front
/// and extracting handles from a half-built pipe in the right order.
pub fn start_session<T, U>(
    pty: T,
    event_proxy: U,
    options: SessionOptions,
) -> Result<SessionHandles, Box<dyn error::Error>>
where
    T: EventedPty + Send + 'static,
    U: EventListener + Send + 'static,
{
    let render_buffer = Arc::new(FrameStore::new(RenderBuffer::new(
        options.cols.max(1) as usize,
        options.rows.max(1) as usize,
    )));

    let vt_modes = Arc::new(AtomicU32::new(0));

    let mut pipe = PtyPipe::new(
        Arc::clone(&render_buffer),
        Arc::clone(&vt_modes),
        pty,
        event_proxy,
        &options,
    )?;

    // Configure the engine before transferring exclusive ownership to the
    // event-loop thread, so the first publication uses the requested cursor.
    pipe.ghostty
        .set_default_cursor_shape(options.cursor_shape)
        .map_err(|error| Box::new(error) as Box<dyn error::Error>)?;

    pipe.terminal_responses_enabled = options.terminal_responses;
    pipe.output_sink = options.output_sink;

    pipe.ghostty
        .snapshot_into(&mut pipe.back_buffer, 0, 0)
        .map_err(|error| Box::new(error) as Box<dyn error::Error>)?;

    render_buffer.publish(&mut pipe.back_buffer);

    let messenger = pipe.channel();

    let thread = Builder::new().name("PTY reader".into()).spawn(move || {
        pipe.run_event_loop();
    })?;

    Ok(SessionHandles {
        worker: SessionWorker {
            messenger: messenger.clone(),
            thread: Some(thread),
        },
        render_buffer,
        vt_modes,
        messenger,
    })
}
