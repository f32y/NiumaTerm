//! One terminal surface's runtime state: libghostty-vt engine, render buffer, and
//! the ConPTY-backed PTY worker so platform details stay outside the UI layer.

use std::collections::VecDeque;
use std::error::Error as StdError;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time;

use nmt_config::colors::Colors;
use nmt_platform::process::ProcessTree;
use nmt_platform::{
    EventedPty, PtyOptions, WinsizeBuilder, create_managed_pty_with_env, create_pty_with_env,
};
use parking_lot::Mutex;
use tracing::error;

use crate::block_store::BlockStore;
use crate::event::{BlockEvent, Msg, MsgSender, ProgressReport};
use crate::pty_pipe::{SessionOptions, start_session};
use crate::publication::FrameStore;
pub use crate::session::error::{EngineError, EngineErrorCode};
pub use crate::session::mouse::{
    SurfaceCell, SurfaceCellSide, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
};
pub use crate::session::observer::{SessionChange, SessionObserver};
use crate::session::page::PageCache;
use crate::session::selection::SurfaceSelection;

mod blocks;
pub mod page;
pub mod request;
mod rows;
pub use crate::session::blocks::BlockPoint;
pub use crate::session::rows::RowText;

mod config;
mod error;
mod input;
pub mod interaction;
mod mouse;
mod observer;
mod proxy;
mod reads;
mod scroll;
pub(crate) mod selection;

pub use crate::session::config::TerminalSessionConfig;
use crate::session::config::default_shell;
use crate::session::proxy::TerminalEventProxy;

type SessionBuffer = Arc<FrameStore>;

/// A host event surfaced from the PTY thread to the shell. The shell
/// drains these on its render tick via [`TerminalSession::poll_events`].
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

/// The host's session handle: commands, immutable frame publications, and
/// asynchronous reads. The PTY event loop exclusively owns the engine.
pub struct TerminalSession {
    pages: Mutex<PageCache>,
    render_buffer: SessionBuffer,
    vt_modes: Arc<AtomicU32>,
    messenger: MsgSender,
    shared: Arc<SessionSharedState>,
    process_tree: Option<ProcessTree>,
    /// Engine-blocks mode is active: frozen history lives in
    /// finished engine blocks, read through owned pages. Mirrors the flag the
    /// PTY event loop runs with.
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
    /// The in-flight command, if one is executing.
    in_flight: Mutex<Option<InFlightBlock>>,
    open_prompt: Mutex<bool>,
    read_only: AtomicBool,
    exited: AtomicBool,
    alt_screen: AtomicBool,
    selection: SurfaceSelection,
    /// Block events wait for the read-cycle damage notification so image
    /// generations are installed before frozen rows become visible.
    staged_blocks: Mutex<Vec<BlockEvent>>,
}

impl TerminalSession {
    /// Create a terminal session and start its shell through the platform PTY.
    /// `id` identifies engine events. The optional observer receives synchronous
    /// presentation updates without owning the session.
    pub fn new(
        config: &TerminalSessionConfig,
        id: u64,
        colors: Colors,
        observer: Option<Arc<dyn SessionObserver>>,
    ) -> Result<TerminalSession, EngineError> {
        let shell = config.shell.clone().unwrap_or_else(default_shell);
        let cols = config.cols.max(1);
        let rows = config.rows.max(1);

        let pty_options = PtyOptions {
            shell: &shell,
            args: &config.args,
            working_directory: config.working_dir.as_deref(),
            columns: cols,
            rows,
            environment_overrides: &config.environment_overrides,
            starting_title: config.starting_title.as_deref(),
            bootstrap: config.bootstrap.as_deref(),
        };

        let pty = if config.manage_process_tree {
            create_managed_pty_with_env(pty_options)
        } else {
            create_pty_with_env(pty_options)
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
                colors,
                cursor_shape: config.cursor_shape,
                scrollback_lines: config.scrollback_lines,
                engine_blocks: config.engine_blocks,
                terminal_responses: true,
                output_sink: None,
            },
            observer,
        )
    }

    pub fn from_pty<T: EventedPty + Send + 'static>(
        pty: T,
        process_tree: Option<ProcessTree>,
        options: SessionOptions,
        observer: Option<Arc<dyn SessionObserver>>,
    ) -> Result<Self, EngineError> {
        let shared = Arc::new(SessionSharedState::default());
        let proxy = TerminalEventProxy::new(Arc::clone(&shared), options.route_id as u64, observer);
        let engine_blocks = options.engine_blocks;
        let handles = start_session(pty, proxy, options).map_err(engine_init_error)?;

        Ok(Self {
            pages: Mutex::new(PageCache::default()),
            render_buffer: handles.render_buffer,
            vt_modes: handles.vt_modes,
            messenger: handles.messenger,
            shared,
            process_tree,
            engine_blocks,
        })
    }

    /// Whether frozen history lives in finished engine blocks.
    pub fn engine_blocks(&self) -> bool {
        self.engine_blocks
    }

    /// Shared frozen block-split history (renderer read side).
    pub fn block_store(&self) -> Arc<Mutex<BlockStore>> {
        Arc::clone(&self.shared.block_store)
    }

    /// Number of processes beyond the shell itself in the shell's Job
    /// Object (requires job management; 0 otherwise).
    pub fn child_process_count(&self) -> usize {
        self.process_tree
            .as_ref()
            .map_or(0, ProcessTree::other_process_count)
    }

    /// Write input bytes (already terminal-encoded) to the session's PTY.
    /// True means the local queue accepted the bytes, not that a remote peer
    /// received them or the shell processed them.
    pub fn write_input(&self, data: &[u8]) -> bool {
        if data.is_empty()
            || self.shared.read_only.load(Ordering::Acquire)
            || self.shared.exited.load(Ordering::Acquire)
        {
            return false;
        }
        self.messenger
            .send(Msg::Input(data.to_vec().into()))
            .is_ok()
    }

    pub fn resize(&self, cols: u16, rows: u16, width: u16, height: u16) -> bool {
        if self.shared.exited.load(Ordering::Acquire) {
            return false;
        }
        self.messenger
            .send(Msg::Resize(WinsizeBuilder {
                cols,
                rows,
                width,
                height,
            }))
            .is_ok()
    }

    pub fn exited(&self) -> bool {
        self.shared.exited.load(Ordering::Acquire)
    }

    pub fn alt_screen(&self) -> bool {
        self.shared.alt_screen.load(Ordering::Acquire)
    }

    pub fn mark_read_only(&self) {
        self.shared.read_only.store(true, Ordering::Release);
    }

    pub fn current_directory(&self) -> Option<String> {
        self.render_buffer.load().current_directory.clone()
    }

    /// Drain host events in their publication order. Directory changes are
    /// captured by the engine owner alongside other metadata.
    pub fn poll_events(&self) -> Vec<HostEvent> {
        let mut out = Vec::new();
        let mut q = self.shared.events.lock();

        while let Some(e) = q.pop_front() {
            out.push(e);
        }

        drop(q);

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
mod interaction_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vtebench_tests;

#[cfg(test)]
mod block_tests;
