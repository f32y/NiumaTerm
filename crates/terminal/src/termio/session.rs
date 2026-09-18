use std::error;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_platform::AsyncPty;
use tokio::task::JoinHandle;

use crate::event::{EventListener, Msg, MsgSender};
use crate::render_buffer::{FrameStore, RenderBuffer};
use crate::termio::Termio;

/// Observes the exact VT bytes accepted by the engine, in the owner task.
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

    /// Sender for input, resize, and shutdown messages to the PTY task.
    pub messenger: MsgSender,
}

/// Owns the async PTY task. Dropping requests shutdown; explicit shutdown
/// awaits final publication and native cleanup without blocking a runtime worker.
pub struct SessionWorker {
    messenger: MsgSender,
    task: Option<JoinHandle<()>>,
}

#[cfg(test)]
impl SessionWorker {
    pub(crate) fn detached_for_test(messenger: MsgSender) -> Self {
        Self {
            messenger,
            task: None,
        }
    }
}

impl SessionWorker {
    pub async fn shutdown(mut self) {
        let task = self.task.take();

        // Dropping requests the shutdown; waiting for the task afterwards is
        // what distinguishes this from an implicit drop.
        drop(self);

        if let Some(task) = task
            && let Err(error) = task.await
        {
            tracing::warn!(%error, "PTY task did not finish normally");
        }
    }
}

impl Drop for SessionWorker {
    fn drop(&mut self) {
        // The task owns native resources until cleanup finishes. Awaiting it
        // here could deadlock the runtime that must process this shutdown.
        let _ = self.messenger.send(Msg::Shutdown);
    }
}

impl SessionHandles {
    pub async fn shutdown(self) {
        self.worker.shutdown().await;
    }
}

/// Build the engine and render buffer, configure the pipe, and start the PTY
/// async task. The single construction entry point: callers receive
/// every shared handle from one call instead of assembling buffers up front
/// and extracting handles from a half-built pipe in the right order.
pub fn start_session<T, U>(
    pty: T,
    event_proxy: U,
    options: SessionOptions,
) -> Result<SessionHandles, Box<dyn error::Error>>
where
    T: AsyncPty + Send + 'static,
    U: EventListener + Send + 'static,
{
    let render_buffer = Arc::new(FrameStore::new(RenderBuffer::new(
        options.cols.max(1) as usize,
        options.rows.max(1) as usize,
    )));

    let vt_modes = Arc::new(AtomicU32::new(0));

    let mut pipe = Termio::new(
        Arc::clone(&render_buffer),
        Arc::clone(&vt_modes),
        pty,
        event_proxy,
        &options,
    )?;

    // Configure the engine before transferring exclusive ownership to the
    // event-loop task, so the first publication uses the requested cursor.
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

    let task = nmt_runtime::handle().spawn(async move {
        let finished = pipe.run_event_loop().await;

        // Dropping the PTY unregisters native wait callbacks, which waits for
        // any callback still running, and sees a cancelled overlapped write
        // through to completion; freeing the engine's scrollback is finite CPU
        // work. Neither belongs on an I/O worker.
        if let Err(error) = nmt_runtime::handle()
            .spawn_blocking(move || drop(finished))
            .await
        {
            tracing::warn!(%error, "PTY cleanup did not finish normally");
        }
    });

    Ok(SessionHandles {
        worker: SessionWorker {
            messenger: messenger.clone(),
            task: Some(task),
        },
        render_buffer,
        vt_modes,
        messenger,
    })
}
