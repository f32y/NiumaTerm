//! One terminal surface's runtime state: libghostty-vt engine, render buffer, and
//! the ConPTY-backed PTY worker so platform details stay outside the UI layer.

use std::collections::{self, VecDeque};
use std::error::Error as StdError;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time;

use nmt_config::{CursorShape, active_colors};
use nmt_platform::{WinsizeBuilder, create_pty_with_env, job_other_process_count};
use nmt_terminal::block_store::BlockStore;
use nmt_terminal::event::{Msg, MsgSender, ProgressReport};
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::pty_pipe::{SessionOptions, start_session};
use nmt_terminal::render_buffer::RenderBuffer;
use parking_lot::{FairMutex, Mutex};
use tracing::error;

use crate::error::{EngineError, EngineErrorCode};
use crate::terminal;
use crate::terminal::graphics::GenerationStore;
use crate::terminal::wake::WakeSender;

mod config;
mod proxy;

use config::default_shell;

pub use crate::terminal::session::config::TerminalSessionConfig;
pub use crate::terminal::session::proxy::TerminalEventProxy;

pub(crate) type SessionGraphics = Arc<Mutex<GenerationStore>>;

pub(crate) type SessionEngine = Arc<FairMutex<GhosttyTerminal>>;
pub(crate) type SessionBuffer = Arc<FairMutex<RenderBuffer>>;
pub(crate) type HostEventQueue = Arc<Mutex<VecDeque<HostEvent>>>;

/// A host event surfaced from the PTY thread to the shell. The shell
/// drains these on its render tick via [`Session::poll_events`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// Terminal title changed (OSC 0/2).
    Title(String),
    /// Bell (BEL).
    Bell,
    /// Progress report (OSC 9;4) from a long-running command.
    Progress(ProgressReport),
    /// The shell process exited.
    Exit,
    /// Working directory changed (OSC 7).
    Cwd(String),
    /// Desktop notification (OSC 9 / OSC 777).
    Notification { title: String, body: String },
    /// Entered (`true`) or left (`false`) an interactive full-screen program.
    InteractiveState(bool),
    /// A full-screen program entered (`true`) or left (`false`) the alt-screen — a
    /// subset of [`Self::InteractiveState`] that gates command-block chrome.
    AltScreen(bool),
    /// The integrated shell boundary lifecycle is trusted for fixed-bottom
    /// prompt ownership.
    PromptBoundaryTrusted(bool),
    /// A trusted integrated-shell prompt region is open.
    PromptStarted,
    /// Integrated-shell command metadata changed in the block store.
    CommandFinished,
    /// An integrated-shell command began executing; read `in_flight_block`.
    CommandStarted,
}

/// The currently executing command for split live chrome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlightBlock {
    pub command: String,
    pub started_at: time::SystemTime,
}

/// One terminal surface's runtime state — a thin headless bundle over terminal's
/// `PtyPipe`. The PTY thread parses ConPTY output into the engine + render buffer;
/// the render host reads the buffer under its own lock.
pub struct TerminalSession {
    pub(crate) engine: SessionEngine,
    pub(crate) render_buffer: SessionBuffer,
    pub(crate) vt_modes: Arc<AtomicU32>,
    pub(crate) messenger: MsgSender,
    pub(crate) events: HostEventQueue,
    /// Frozen block-split history; read side of the block-event pipeline.
    block_store: Arc<Mutex<BlockStore>>,
    /// Live Kitty image generations keyed by image ID. Written on the PTY
    /// thread from `UpdateGraphics`; read by the pane/frame path for paint.
    generation_store: SessionGraphics,
    /// Lock-free mirror of the live generation count. The render path reads this to
    /// skip the generation-store lock/clone entirely when no images exist — so a
    /// graphics-free session pays nothing per frame.
    live_image_count: Arc<AtomicUsize>,
    /// The in-flight command, if one is executing.
    in_flight: Arc<Mutex<Option<InFlightBlock>>>,
    open_prompt: Arc<Mutex<bool>>,
    /// Lazily-read frozen Kitty generations keyed `(block_id, image_id)`;
    /// lives beside the (gpui-free) block store because the values are gpui
    /// images. Pruned by the proxy on the same batches that feed the store.
    frozen_images: terminal::graphics::FrozenImageCache,
    /// Raw Job Object handle managing the shell tree (when job management was
    /// on at spawn). Owned by the PTY; only queried while the session lives.
    job_handle: Option<isize>,
    /// Engine-blocks mode is active: frozen history lives in
    /// finished engine blocks, rendered through `BlockRef` handles. Mirrors the
    /// flag the PTY pipe runs with.
    engine_blocks: bool,
}

impl TerminalSession {
    /// Create a terminal session and start its shell through ConPTY. `id` is the
    /// per-surface unique identity used as the engine's event route id (multi-tab
    /// safety). `wake` signals the shell's render loop on PTY damage / host events
    /// to coalesce render work; `None` runs headless without render driving.
    pub fn new(
        config: &TerminalSessionConfig,
        id: u64,
        wake: Option<WakeSender>,
    ) -> Result<TerminalSession, EngineError> {
        Self::new_internal(config, id, wake)
    }

    /// Create a terminal session backed by a remote session instead of a local
    /// ConPTY. The attach snapshot primes the screen; live output, input, and
    /// resize flow over the network through `NetPty`. Every other layer (engine,
    /// proxy, render buffer, wake) is identical to a local session.
    #[cfg(windows)]
    pub fn new_remote(
        remote: nmt_remote_net::RemoteSession,
        id: u64,
        wake: Option<WakeSender>,
    ) -> Result<TerminalSession, EngineError> {
        use crate::terminal::net_pty::NetPty;

        let snapshot = remote.snapshot();
        let cols = snapshot.cols.max(1);
        let rows = snapshot.rows.max(1);
        let events: HostEventQueue = Arc::new(Mutex::new(collections::VecDeque::new()));
        let block_store: Arc<Mutex<BlockStore>> = Arc::new(Mutex::new(BlockStore::default()));
        let generation_store: SessionGraphics = Arc::new(Mutex::new(GenerationStore::new()));
        let live_image_count = Arc::new(AtomicUsize::new(0));
        let in_flight: Arc<Mutex<Option<InFlightBlock>>> = Arc::new(Mutex::new(None));
        let open_prompt: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let frozen_images: terminal::graphics::FrozenImageCache = Default::default();

        let engine_blocks = false;
        let proxy = TerminalEventProxy::new(
            Arc::clone(&events),
            Arc::clone(&block_store),
            Arc::clone(&generation_store),
            Arc::clone(&live_image_count),
            Arc::new(Mutex::new(Vec::new())),
            Arc::clone(&in_flight),
            Arc::clone(&open_prompt),
            Arc::clone(&frozen_images),
            id,
            wake,
        );

        let pty = NetPty::new(remote);

        let handles = start_session(
            pty,
            proxy,
            SessionOptions {
                cols,
                rows,
                route_id: id as usize,
                colors: active_colors(),
                cursor_shape: CursorShape::Block,
                scrollback_lines: 10_000,
                engine_blocks,
                terminal_responses: true,
                output_sink: None,
            },
        )
        .map_err(engine_init_error)?;

        Ok(TerminalSession {
            engine: handles.engine,
            render_buffer: handles.render_buffer,
            vt_modes: handles.vt_modes,
            messenger: handles.messenger,
            events,
            block_store,
            generation_store,
            live_image_count,
            in_flight,
            open_prompt,
            frozen_images,
            job_handle: None,
            engine_blocks,
        })
    }

    fn new_internal(
        config: &TerminalSessionConfig,
        id: u64,
        wake: Option<WakeSender>,
    ) -> Result<TerminalSession, EngineError> {
        let shell = config.shell.clone().unwrap_or_else(default_shell);
        let cols = config.cols.max(1);
        let rows = config.rows.max(1);
        let events: HostEventQueue = Arc::new(Mutex::new(collections::VecDeque::new()));
        let block_store: Arc<Mutex<BlockStore>> = Arc::new(Mutex::new(BlockStore::default()));
        let generation_store: SessionGraphics = Arc::new(Mutex::new(GenerationStore::new()));
        let live_image_count = Arc::new(AtomicUsize::new(0));
        let in_flight: Arc<Mutex<Option<InFlightBlock>>> = Arc::new(Mutex::new(None));
        let open_prompt: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let frozen_images: terminal::graphics::FrozenImageCache = Default::default();

        let engine_blocks = config.engine_blocks;
        let proxy = TerminalEventProxy::new(
            Arc::clone(&events),
            Arc::clone(&block_store),
            Arc::clone(&generation_store),
            Arc::clone(&live_image_count),
            Arc::new(Mutex::new(Vec::new())),
            Arc::clone(&in_flight),
            Arc::clone(&open_prompt),
            Arc::clone(&frozen_images),
            id,
            wake,
        );

        let pty = create_pty_with_env(
            &shell,
            config.args.clone(),
            &config.working_dir,
            cols,
            rows,
            &config.environment_overrides,
            config.starting_title.as_deref(),
        )
        .map_err(|error| {
            error!("session create_pty failed: {error:?}");
            EngineError::new(
                EngineErrorCode::PtySpawn,
                format!("failed to start shell '{shell}' via ConPTY: {error}"),
            )
        })?;

        let job_handle = pty.job_handle().map(|handle| handle as isize);

        let handles = start_session(
            pty,
            proxy,
            SessionOptions {
                cols,
                rows,
                route_id: id as usize,
                colors: active_colors(),
                cursor_shape: config.cursor_shape,
                scrollback_lines: config.scrollback_lines,
                engine_blocks,
                terminal_responses: true,
                output_sink: None,
            },
        )
        .map_err(engine_init_error)?;

        Ok(TerminalSession {
            engine: handles.engine,
            render_buffer: handles.render_buffer,
            vt_modes: handles.vt_modes,
            messenger: handles.messenger,
            events,
            block_store,
            generation_store,
            live_image_count,
            in_flight,
            open_prompt,
            frozen_images,
            job_handle,
            engine_blocks,
        })
    }

    /// Whether frozen history lives in finished engine blocks.
    pub(crate) fn engine_blocks(&self) -> bool {
        self.engine_blocks
    }

    /// Shared frozen block-split history (renderer read side).
    pub(crate) fn block_store(&self) -> Arc<Mutex<BlockStore>> {
        Arc::clone(&self.block_store)
    }

    /// Shared frozen Kitty generation cache (renderer read/insert side).
    pub(crate) fn frozen_images(&self) -> terminal::graphics::FrozenImageCache {
        Arc::clone(&self.frozen_images)
    }

    /// Shared live Kitty image generations for pane and frame painting.
    pub(crate) fn generation_store(&self) -> SessionGraphics {
        Arc::clone(&self.generation_store)
    }

    /// Whether any live Kitty image generation exists (lock-free). The render path
    /// uses this to avoid touching the generation store when graphics are unused.
    pub(crate) fn has_live_images(&self) -> bool {
        self.live_image_count.load(Ordering::Relaxed) != 0
    }

    /// Number of processes beyond the shell itself in the shell's Job
    /// Object (requires job management; 0 otherwise).
    pub fn child_process_count(&self) -> usize {
        self.job_handle.map_or(0, job_other_process_count)
    }

    /// Write input bytes (already terminal-encoded) to the session's PTY.
    pub fn write_input(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let _ = self.messenger.send(Msg::Input(data.to_vec().into()));
    }

    pub fn resize(&self, cols: u16, rows: u16, width: u16, height: u16) {
        let _ = self.messenger.send(Msg::Resize(WinsizeBuilder {
            cols,
            rows,
            width,
            height,
        }));
    }

    /// Drain all pending host events. Queued events (title/bell/exit/
    /// …) drain first in order; then, if the working directory changed, a trailing
    /// `Cwd` event is appended (OSC 7 updates state rather than emitting a TerminalEvent).
    pub fn poll_events(&self) -> Vec<HostEvent> {
        let mut out = Vec::new();
        let mut q = self.events.lock();

        while let Some(e) = q.pop_front() {
            out.push(e);
        }

        drop(q);

        if let Some(pwd) = self.engine.lock().poll_pwd() {
            out.push(HostEvent::Cwd(pwd));
        }

        out
    }

    pub fn in_flight_block(&self) -> Option<InFlightBlock> {
        self.in_flight.lock().clone()
    }

    pub fn open_prompt_region(&self) -> bool {
        *self.open_prompt.lock()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.messenger.send(Msg::Shutdown);
    }
}

fn engine_init_error(error: Box<dyn StdError>) -> EngineError {
    error!("session start failed: {error:?}");
    EngineError::new(
        EngineErrorCode::EngineInit,
        format!("libghostty-vt engine init failed: {error}"),
    )
}

#[cfg(test)]
mod tests;
