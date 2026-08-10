use std::error;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use nmt_config::colors::Colors;
use nmt_platform::EventedPty;
use parking_lot::FairMutex;

use crate::ansi::CursorShape;
use crate::event::{EventListener, MsgSender, WindowId};
use crate::ghostty::GhosttyTerminal;
use crate::pty_pipe::PtyPipe;
use crate::render_buffer::RenderBuffer;

/// Observer for the exact VT byte stream accepted by the engine. Runs under
/// the engine lock so a checkpoint and its following bytes order atomically;
/// observers must return promptly.
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
    /// VT engine. The PTY thread and the frontend serialize through its lock.
    pub engine: Arc<FairMutex<GhosttyTerminal>>,
    /// Viewport copy the renderer reads; on its own lock, separate from the
    /// engine, so paint never waits behind a parse.
    pub render_buffer: Arc<FairMutex<RenderBuffer>>,
    /// VT modes published by the pipe; the input path reads them lock-free.
    pub vt_modes: Arc<AtomicU32>,
    /// Sender for input, resize, and shutdown messages to the PTY thread.
    pub messenger: MsgSender,
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
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(
        options.cols.max(1) as usize,
        options.rows.max(1) as usize,
    )));
    let vt_modes = Arc::new(AtomicU32::new(0));

    let mut pipe = PtyPipe::new(
        Arc::clone(&render_buffer),
        Arc::clone(&vt_modes),
        pty,
        event_proxy,
        WindowId::dummy(),
        options.route_id,
        options.colors,
        options.scrollback_lines,
        options.engine_blocks,
    )?;

    // The pipe has not spawned yet, so the engine lock is uncontended and the
    // cursor shape lands before the first PTY byte can be parsed.
    pipe.ghostty
        .lock()
        .set_default_cursor_shape(options.cursor_shape)
        .map_err(|error| Box::new(error) as Box<dyn error::Error>)?;

    pipe.terminal_responses_enabled = options.terminal_responses;
    pipe.output_sink = options.output_sink;

    let engine = Arc::clone(&pipe.ghostty);
    let messenger = pipe.channel();

    drop(pipe.spawn());

    Ok(SessionHandles {
        engine,
        render_buffer,
        vt_modes,
        messenger,
    })
}
