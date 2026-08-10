//! One terminal surface's runtime state: libghostty-vt engine, render buffer, and
//! the ConPTY-backed PTY worker so platform details stay outside the UI layer.

use std::collections::{self, VecDeque};
use std::error::Error as StdError;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::{mem, time};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use nmt_config::local_state::TabState;
use nmt_config::{CursorShape, active_colors};
use nmt_platform::{WinsizeBuilder, create_pty_with_env, job_other_process_count};
use nmt_terminal::block_store::{BlockStore, SegmentMeta};
use nmt_terminal::clipboard::Clipboard;
use nmt_terminal::event::{
    BlockEvent, EventListener, Msg, MsgSender, ProgressReport, TerminalEvent, WindowId,
};
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::pty_pipe::{SessionOptions, start_session};
use nmt_terminal::render_buffer::RenderBuffer;
use parking_lot::{FairMutex, Mutex};
use tracing::{debug, error};

use crate::error::{EngineError, EngineErrorCode};
use crate::terminal;
use crate::terminal::graphics::GenerationStore;
use crate::terminal::wake::{Wake, WakeSender};
use crate::utils::POWERSHELL_INTEGRATION;

static ENCODED_POWERSHELL_INTEGRATION: LazyLock<String> =
    LazyLock::new(|| encode_powershell_command(POWERSHELL_INTEGRATION));

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
        let proxy = TerminalEventProxy {
            events: Arc::clone(&events),
            block_store: Arc::clone(&block_store),
            generation_store: Arc::clone(&generation_store),
            live_image_count: Arc::clone(&live_image_count),
            staged_blocks: Arc::new(Mutex::new(Vec::new())),
            in_flight: Arc::clone(&in_flight),
            open_prompt: Arc::clone(&open_prompt),
            frozen_images: Arc::clone(&frozen_images),
            id,
            wake,
        };

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
        let proxy = TerminalEventProxy {
            events: Arc::clone(&events),
            block_store: Arc::clone(&block_store),
            generation_store: Arc::clone(&generation_store),
            live_image_count: Arc::clone(&live_image_count),
            staged_blocks: Arc::new(Mutex::new(Vec::new())),
            in_flight: Arc::clone(&in_flight),
            open_prompt: Arc::clone(&open_prompt),
            frozen_images: Arc::clone(&frozen_images),
            id,
            wake,
        };

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

pub(crate) fn default_shell() -> String {
    "powershell.exe".to_string()
}

/// Whether a configured shell is PowerShell (the only shell we have an OSC 133
/// integration script for). `None` resolves to the PowerShell default.
pub(crate) fn shell_is_powershell(shell: Option<&str>) -> bool {
    match shell {
        Some(s) => {
            let lower = s.to_ascii_lowercase();
            lower.contains("powershell") || lower.contains("pwsh")
        }
        None => true,
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

#[derive(Clone)]
pub struct TerminalEventProxy {
    events: HostEventQueue,
    /// Frozen block-split history (engine-block handles + command
    /// metadata); shared with `TerminalSession`. Fed on the PTY thread.
    block_store: Arc<Mutex<BlockStore>>,
    /// Live Kitty image generations keyed by image ID; shared with the session
    /// and pane. `UpdateGraphics` installs/removes generations here.
    generation_store: SessionGraphics,
    /// Lock-free mirror of the live generation count (shared with the session), kept
    /// in sync whenever `UpdateGraphics` installs or removes generations.
    live_image_count: Arc<AtomicUsize>,
    /// The current PTY read's block events, staged until that read's damage
    /// wake so image generations install before frozen slices bind.
    /// Bounded to one read cycle; flushed or discarded on damage/shutdown.
    staged_blocks: Arc<Mutex<Vec<BlockEvent>>>,
    /// The in-flight command; set at CommandStarted, cleared by
    /// CommandFinished / trust loss / exit.
    in_flight: Arc<Mutex<Option<InFlightBlock>>>,
    open_prompt: Arc<Mutex<bool>>,
    /// Frozen Kitty generation cache, pruned on the same batches that feed
    /// the block store (evicted blocks drop their cached images).
    frozen_images: terminal::graphics::FrozenImageCache,
    /// Source surface id, stamped onto every wake so the shell can route by tab.
    id: u64,
    /// Render-wakeup sender; `None` for sessions/tests without a live shell.
    wake: Option<WakeSender>,
}

impl TerminalEventProxy {
    fn signal(&self, kind: Wake) {
        if let Some(tx) = &self.wake {
            tx.send(kind);
        }
    }

    /// Flush the current read's staged block events into the block store.
    /// Called on the read's damage wake so items land together with the
    /// render they belong to. Empty in steady state.
    fn flush_staged_blocks(&self) {
        let batch = mem::take(&mut *self.staged_blocks.lock());
        if batch.is_empty() {
            return;
        }
        terminal::graphics::prune_frozen_images(&self.frozen_images, &batch);
        self.block_store.lock().apply(batch);
    }
}

impl EventListener for TerminalEventProxy {
    fn event(&self) -> (Option<TerminalEvent>, bool) {
        (None, false)
    }

    fn send_event(&self, event: TerminalEvent, _id: WindowId) {
        // Content damage drives a render with no chrome rebuild. This is the read's
        // final wake: image generations have already installed (UpdateGraphics runs
        // before this), so flush the staged block events before the UI
        // wakes.
        if matches!(
            event,
            TerminalEvent::TerminalDamaged(_) | TerminalEvent::Render
        ) {
            self.flush_staged_blocks();
            self.signal(Wake::Content(self.id));

            return;
        }

        // Decoded Kitty pixels: install/replace live generations and drop removed
        // ones, then wake for a lazy visible upload. Route-scoped so a cross-session
        // event is ignored.
        if let TerminalEvent::UpdateGraphics { route_id, queues } = event {
            if route_id != self.id as usize {
                return;
            }

            let mut store = self.generation_store.lock();

            for (image_id, data) in queues.pending_images {
                store.install(image_id, data);
            }

            for gid in queues.remove_queue {
                store.remove(gid.0 as u32);
            }

            // Publish the live count lock-free so the render path can skip the store.
            self.live_image_count.store(store.len(), Ordering::Relaxed);

            drop(store);

            self.signal(Wake::Content(self.id));

            return;
        }
        let host = match event {
            TerminalEvent::Title(t) | TerminalEvent::TitleWithSubtitle(t, _) => HostEvent::Title(t),
            TerminalEvent::ResetTitle => HostEvent::Title(String::new()),
            TerminalEvent::Bell => HostEvent::Bell,
            TerminalEvent::ProgressReport(report) => HostEvent::Progress(report),
            TerminalEvent::ClipboardStore(ty, text) => {
                let mut clipboard = Clipboard::default();

                clipboard.set(ty, text);

                return;
            }
            TerminalEvent::CloseTerminal(_) => {
                // The shell died: no ;D is coming for a running command, and any
                // half-staged block batch for the interrupted read is discarded.
                *self.in_flight.lock() = None;

                *self.open_prompt.lock() = false;

                self.staged_blocks.lock().clear();

                HostEvent::Exit
            }
            TerminalEvent::DesktopNotification { title, body } => {
                HostEvent::Notification { title, body }
            }
            TerminalEvent::InteractiveState(on) => HostEvent::InteractiveState(on),
            TerminalEvent::AltScreen(on) => HostEvent::AltScreen(on),
            TerminalEvent::PromptBoundaryTrusted(on) => {
                if !on {
                    // Trust lost mid-command (nested shell, malformed stream): the
                    // running block's lifecycle can no longer complete.
                    *self.in_flight.lock() = None;
                    *self.open_prompt.lock() = false;
                }

                HostEvent::PromptBoundaryTrusted(on)
            }
            TerminalEvent::PromptStarted => {
                *self.open_prompt.lock() = true;

                HostEvent::PromptStarted
            }
            TerminalEvent::BlockBatch(batch) => {
                // Stage this read's block events; they flush to the store on the
                // read's damage wake, after `UpdateGraphics` installs the generations
                // its slices bind to. No chrome/content wake here.
                self.staged_blocks.lock().extend(batch);

                return;
            }
            TerminalEvent::CommandStarted(cmd) => {
                *self.open_prompt.lock() = false;

                // Marry the command metadata to its block item; the
                // segment materializes later, when its rows scroll out.
                self.block_store
                    .lock()
                    .update_meta(cmd.seq, |m: &mut SegmentMeta| {
                        m.command = Some(cmd.command.clone());
                        m.cwd = cmd.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
                        m.started_at = Some(cmd.started_at);
                    });

                let block = InFlightBlock {
                    command: cmd.command,
                    started_at: cmd.started_at,
                };

                *self.in_flight.lock() = Some(block);

                HostEvent::CommandStarted
            }
            TerminalEvent::CommandFinished(cmd) => {
                *self.in_flight.lock() = None;

                self.block_store
                    .lock()
                    .update_meta(cmd.seq, |m: &mut SegmentMeta| {
                        m.exit_code = cmd.exit_code;
                        m.ended_at = Some(cmd.ended_at);
                    });

                debug!(
                    command = %cmd.command,
                    exit_code = ?cmd.exit_code,
                    "command block metadata recorded"
                );

                HostEvent::CommandFinished
            }
            _ => return,
        };

        self.events.lock().push_back(host);

        // A user-visible event changes chrome (tab title, status, attention).
        self.signal(Wake::Chrome(self.id));
    }
}

/// Local terminal session configuration. `None` and empty fields fall back to
/// defaults (`shell` → `powershell.exe`).
#[derive(Debug, Clone)]
pub struct TerminalSessionConfig {
    pub shell: Option<String>,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub starting_title: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// Default cursor shape until the running program selects one with DECSCUSR.
    pub cursor_shape: CursorShape,
    /// Scrollback budget in lines; converted to the engine's byte budget.
    pub scrollback_lines: usize,
    /// Engine-blocks mode is the default because completed commands can freeze
    /// into engine-side blocks at each trusted `;D`; rendering reads
    /// them through `BlockRef` handles. `false` is the internal classic-grid
    /// fallback: no freezing, no boundary clears, no block events, intact
    /// scrollback. The GPUI app keeps this enabled and toggles block chrome only.
    pub engine_blocks: bool,
    /// Child-only values merged into the shell's inherited Windows environment.
    /// Runtime metadata is deliberately excluded from persisted tab state.
    pub environment_overrides: Vec<(String, String)>,
}

impl TerminalSessionConfig {
    pub(crate) fn restorable_tab_state(&self) -> TabState {
        TabState {
            name: None,
            user_named: false,
            shell: self.shell.clone(),
            args: self.args.clone(),
            cwd: self.working_dir.clone(),
            agent: None,
            agent_profile: None,
            panes: None,
        }
    }

    /// Augment a session config so a PowerShell shell evaluates the bundled OSC 133
    /// integration at startup. Only applied to a PowerShell shell with no caller-supplied
    /// args, so explicit args (and non-PowerShell shells) are left untouched.
    pub(crate) fn with_shell_integration(mut self: TerminalSessionConfig) -> TerminalSessionConfig {
        if !self.has_trusted_prompt_integration() {
            return self;
        }

        self.args = vec![
            "-NoExit".to_string(),
            "-EncodedCommand".to_string(),
            (*ENCODED_POWERSHELL_INTEGRATION).clone(),
        ];

        self
    }

    pub(crate) fn has_trusted_prompt_integration(&self) -> bool {
        self.args.is_empty() && shell_is_powershell(self.shell.as_deref())
    }
}

fn encode_powershell_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();

    STANDARD.encode(bytes)
}

impl Default for TerminalSessionConfig {
    fn default() -> Self {
        TerminalSessionConfig {
            shell: None,
            args: Vec::new(),
            working_dir: None,
            starting_title: None,
            cols: 80,
            rows: 24,
            cursor_shape: CursorShape::Block,
            scrollback_lines: 10_000,
            engine_blocks: true,
            environment_overrides: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
