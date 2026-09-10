//! One terminal surface's runtime state: libghostty-vt engine, render buffer, and
//! the ConPTY-backed PTY worker so platform details stay outside the UI layer.

use std::collections::VecDeque;
use std::error::Error as StdError;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time;

use nmt_config::{CursorShape, active_colors};
use nmt_platform::process::ProcessTree;
use nmt_platform::{EventedPty, WinsizeBuilder, create_managed_pty_with_env, create_pty_with_env};
use nmt_terminal::block_store::BlockStore;
use nmt_terminal::event::{BlockEvent, Msg, MsgSender, ProgressReport};
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::pty_pipe::{SessionOptions, start_session};
use nmt_terminal::render_buffer::RenderBuffer;
use parking_lot::{FairMutex, Mutex};
use tracing::error;

use crate::error::{EngineError, EngineErrorCode};
use crate::graphics::GenerationStore;
use crate::wake::WakeSender;

mod config;
mod proxy;

use crate::graphics;
pub use crate::session::config::TerminalSessionConfig;
use crate::session::config::default_shell;
pub use crate::session::proxy::TerminalEventProxy;

pub(crate) type SessionGraphics = Arc<Mutex<GenerationStore>>;

pub(crate) type SessionEngine = Arc<FairMutex<GhosttyTerminal>>;
pub(crate) type SessionBuffer = Arc<FairMutex<RenderBuffer>>;

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
    /// Integrated-shell command metadata changed in the block store. The exit
    /// code rides along so the chrome can grade the result without reaching
    /// back into the block store for the entry that just landed; a shell that
    /// reports no code yields `None`.
    CommandFinished { exit_code: Option<i32> },
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
    shared: Arc<SessionSharedState>,
    process_tree: Option<ProcessTree>,
    /// Engine-blocks mode is active: frozen history lives in
    /// finished engine blocks, rendered through `BlockRef` handles. Mirrors the
    /// flag the PTY pipe runs with.
    engine_blocks: bool,
}

/// The event proxy and render host share these stores for one session.
/// Grouping their allocation prevents local and remote sessions from creating
/// different sets of queues or publishing state into unrelated stores.
#[derive(Default)]
struct SessionSharedState {
    events: Mutex<VecDeque<HostEvent>>,
    /// Frozen block-split history; read side of the block-event pipeline.
    block_store: Arc<Mutex<BlockStore>>,
    /// Live Kitty image generations keyed by image ID. Written on the PTY
    /// thread from `UpdateGraphics`; read by the pane/frame path for paint.
    generation_store: SessionGraphics,
    /// Lock-free mirror of the live generation count. The render path reads this to
    /// skip the generation-store lock/clone entirely when no images exist — so a
    /// graphics-free session pays nothing per frame.
    live_image_count: AtomicUsize,
    /// The in-flight command, if one is executing.
    in_flight: Mutex<Option<InFlightBlock>>,
    open_prompt: Mutex<bool>,
    /// Lazily-read frozen Kitty generations keyed `(block_id, image_id)`;
    /// lives beside the (gpui-free) block store because the values are gpui
    /// images. Pruned by the proxy on the same batches that feed the store.
    frozen_images: graphics::FrozenImageCache,
    /// Block events wait for the read-cycle damage notification so image
    /// generations are installed before frozen rows become visible.
    staged_blocks: Mutex<Vec<BlockEvent>>,
}

impl TerminalSession {
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
        use crate::net_pty::NetPty;

        let snapshot = remote.snapshot();
        let cols = snapshot.cols.max(1);
        let rows = snapshot.rows.max(1);

        let pty = NetPty::new(remote);

        Self::from_pty(
            pty,
            None,
            SessionOptions {
                cols,
                rows,
                route_id: id as usize,
                colors: active_colors(),
                cursor_shape: CursorShape::Block,
                scrollback_lines: 10_000,
                engine_blocks: false,
                terminal_responses: true,
                output_sink: None,
            },
            id,
            wake,
        )
    }

    /// Create a terminal session and start its shell through the platform PTY.
    /// `id` routes engine events to this surface; `wake` coalesces render work
    /// on PTY damage and host events. `None` runs headless.
    pub fn new(
        config: &TerminalSessionConfig,
        id: u64,
        wake: Option<WakeSender>,
    ) -> Result<TerminalSession, EngineError> {
        let shell = config.shell.clone().unwrap_or_else(default_shell);
        let cols = config.cols.max(1);
        let rows = config.rows.max(1);

        let pty = if config.manage_process_tree {
            create_managed_pty_with_env(
                &shell,
                config.args.clone(),
                &config.working_dir,
                cols,
                rows,
                &config.environment_overrides,
                config.starting_title.as_deref(),
                config.bootstrap.as_deref(),
            )
        } else {
            create_pty_with_env(
                &shell,
                config.args.clone(),
                &config.working_dir,
                cols,
                rows,
                &config.environment_overrides,
                config.starting_title.as_deref(),
                config.bootstrap.as_deref(),
            )
        }
        .map_err(|error| {
            error!("session create_pty failed: {error:?}");
            EngineError::new(
                EngineErrorCode::PtySpawn,
                format!("failed to start shell '{shell}': {error}"),
            )
        })?;

        let process_tree = pty.process_tree();

        Self::from_pty(
            pty,
            process_tree,
            SessionOptions {
                cols,
                rows,
                route_id: id as usize,
                colors: active_colors(),
                cursor_shape: config.cursor_shape,
                scrollback_lines: config.scrollback_lines,
                engine_blocks: config.engine_blocks,
                terminal_responses: true,
                output_sink: None,
            },
            id,
            wake,
        )
    }

    fn from_pty<T: EventedPty + Send + 'static>(
        pty: T,
        process_tree: Option<ProcessTree>,
        options: SessionOptions,
        id: u64,
        wake: Option<WakeSender>,
    ) -> Result<Self, EngineError> {
        let shared = Arc::new(SessionSharedState::default());
        let proxy = TerminalEventProxy::new(Arc::clone(&shared), id, wake);
        let engine_blocks = options.engine_blocks;
        let handles = start_session(pty, proxy, options).map_err(engine_init_error)?;
        Ok(Self {
            engine: handles.engine,
            render_buffer: handles.render_buffer,
            vt_modes: handles.vt_modes,
            messenger: handles.messenger,
            shared,
            process_tree,
            engine_blocks,
        })
    }

    /// Whether frozen history lives in finished engine blocks.
    pub(crate) fn engine_blocks(&self) -> bool {
        self.engine_blocks
    }

    /// Shared frozen block-split history (renderer read side).
    pub(crate) fn block_store(&self) -> Arc<Mutex<BlockStore>> {
        Arc::clone(&self.shared.block_store)
    }

    /// Shared frozen Kitty generation cache (renderer read/insert side).
    pub(crate) fn frozen_images(&self) -> graphics::FrozenImageCache {
        Arc::clone(&self.shared.frozen_images)
    }

    /// Shared live Kitty image generations for pane and frame painting.
    pub(crate) fn generation_store(&self) -> SessionGraphics {
        Arc::clone(&self.shared.generation_store)
    }

    /// Whether any live Kitty image generation exists (lock-free). The render path
    /// uses this to avoid touching the generation store when graphics are unused.
    pub(crate) fn has_live_images(&self) -> bool {
        self.shared.live_image_count.load(Ordering::Relaxed) != 0
    }

    /// Number of processes beyond the shell itself in the shell's Job
    /// Object (requires job management; 0 otherwise).
    pub fn child_process_count(&self) -> usize {
        self.process_tree
            .as_ref()
            .map_or(0, ProcessTree::other_process_count)
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
        let mut q = self.shared.events.lock();

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
        self.shared.in_flight.lock().clone()
    }

    pub fn open_prompt_region(&self) -> bool {
        *self.shared.open_prompt.lock()
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
