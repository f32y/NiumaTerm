//! One terminal surface's runtime state: libghostty-vt engine, render buffer, and
//! the ConPTY-backed PTY worker so platform details stay outside the UI layer.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

use base64::Engine as _;
use nmt_remote_session_hub::{RemotePty, RemoteSessionControl, SessionOptions};
use nmt_terminal::block_store::{BlockStore, SegmentMeta};
use nmt_terminal::event::{BlockEvent, EventListener, Msg, MsgSender, TerminalEvent, WindowId};
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::pty_pipe::PtyPipe;
use nmt_terminal::render_buffer::RenderBuffer;
use parking_lot::{FairMutex, Mutex};

use crate::error::{EngineError, EngineErrorCode};
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
    /// The shell process exited.
    Exit,
    /// Working directory changed (OSC 7).
    Cwd(String),
    /// Desktop notification (OSC 9 / OSC 777).
    Notification { title: String, body: String },
    /// Diagnostic / assistant report.
    Diagnostic(String),
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
    pub started_at: std::time::SystemTime,
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
    frozen_images: crate::terminal::graphics::FrozenImageCache,
    /// Raw Job Object handle managing the shell tree (when job management was
    /// on at spawn). Owned by the PTY; only queried while the session lives.
    job_handle: Option<isize>,
    /// Parent-side control used to query the Job Object owned by SessionHub.
    remote_control: Option<RemoteSessionControl>,
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

    fn new_internal(
        config: &TerminalSessionConfig,
        id: u64,
        wake: Option<WakeSender>,
    ) -> Result<TerminalSession, EngineError> {
        use FairMutex;
        use nmt_terminal::render_buffer::RenderBuffer;

        let shell = config.shell.clone().unwrap_or_else(default_shell);
        let cols = config.cols.max(1);
        let rows = config.rows.max(1);
        let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(
            cols as usize,
            rows as usize,
        )));

        let vt_modes = Arc::new(AtomicU32::new(0));
        // Created before the PtyPipe so the listener and the session share it.
        let events: HostEventQueue = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let block_store: Arc<Mutex<BlockStore>> =
            Arc::new(Mutex::new(nmt_terminal::block_store::BlockStore::default()));
        let generation_store: SessionGraphics = Arc::new(Mutex::new(GenerationStore::new()));
        let live_image_count = Arc::new(AtomicUsize::new(0));
        let in_flight: Arc<Mutex<Option<InFlightBlock>>> = Arc::new(Mutex::new(None));
        let open_prompt: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let frozen_images: crate::terminal::graphics::FrozenImageCache = Default::default();

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

        let (job_handle, remote_control, engine, messenger) = if config.remote_session_enabled {
            let pty = RemotePty::spawn(SessionOptions {
                shell: shell.clone(),
                args: config.args.clone(),
                working_directory: config.working_dir.clone(),
                environment_overrides: config.environment_overrides.clone(),
                starting_title: config.starting_title.clone(),
                cols,
                rows,
                scrollback_lines: config.scrollback_lines,
                manage_process_tree: false,
            })
            .map_err(|error| {
                EngineError::new(
                    EngineErrorCode::PtySpawn,
                    format!("failed to start shell '{shell}' through SessionHub: {error}"),
                )
            })?;

            let remote_control = pty.control();

            let (engine, messenger) = start_pipe(
                Arc::clone(&render_buffer),
                Arc::clone(&vt_modes),
                pty,
                proxy,
                id,
                config.scrollback_lines,
                config.cursor_shape,
                engine_blocks,
            )?;

            (None, Some(remote_control), engine, messenger)
        } else {
            let pty = nmt_platform::create_pty_with_env(
                &shell,
                config.args.clone(),
                &config.working_dir,
                cols,
                rows,
                &config.environment_overrides,
                config.starting_title.as_deref(),
            )
            .map_err(|error| {
                tracing::error!("session create_pty failed: {error:?}");
                EngineError::new(
                    EngineErrorCode::PtySpawn,
                    format!("failed to start shell '{shell}' via ConPTY: {error}"),
                )
            })?;

            let job_handle = pty.job_handle().map(|handle| handle as isize);

            let (engine, messenger) = start_pipe(
                Arc::clone(&render_buffer),
                Arc::clone(&vt_modes),
                pty,
                proxy,
                id,
                config.scrollback_lines,
                config.cursor_shape,
                engine_blocks,
            )?;

            (job_handle, None, engine, messenger)
        };

        Ok(TerminalSession {
            engine,
            render_buffer,
            vt_modes,
            messenger,
            events,
            block_store,
            generation_store,
            live_image_count,
            in_flight,
            open_prompt,
            frozen_images,
            job_handle,
            remote_control,
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
    pub(crate) fn frozen_images(&self) -> crate::terminal::graphics::FrozenImageCache {
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
        if let Some(control) = &self.remote_control {
            return control.child_process_count().unwrap_or_else(|error| {
                // An unavailable count must not silently bypass the destructive
                // close confirmation while the remote shell may still be alive.
                tracing::warn!("failed to query SessionHub child processes: {error}");
                1
            });
        }

        self.job_handle
            .map_or(0, nmt_platform::job_other_process_count)
    }

    /// Write input bytes (already terminal-encoded) to the session's PTY.
    pub fn write_input(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let _ = self.messenger.send(Msg::Input(data.to_vec().into()));
    }

    pub fn resize(&self, cols: u16, rows: u16, width: u16, height: u16) {
        let _ = self
            .messenger
            .send(Msg::Resize(nmt_platform::WinsizeBuilder {
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

fn start_pipe<T>(
    render_buffer: SessionBuffer,
    vt_modes: Arc<AtomicU32>,
    pty: T,
    proxy: TerminalEventProxy,
    id: u64,
    scrollback_lines: usize,
    cursor_shape: nmt_config::CursorShape,
    engine_blocks: bool,
) -> Result<(SessionEngine, MsgSender), EngineError>
where
    T: nmt_platform::EventedPty + Send + 'static,
{
    // The same parser/render pipeline consumes local ConPTY and SessionHub byte
    // streams, keeping terminal semantics independent of process placement.
    let pipe = PtyPipe::new(
        render_buffer,
        vt_modes,
        pty,
        proxy,
        WindowId::dummy(),
        id as usize,
        nmt_config::active_colors(),
        scrollback_lines,
        engine_blocks,
    )
    .map_err(|error| {
        tracing::error!("session PtyPipe::new failed: {error:?}");
        EngineError::new(
            EngineErrorCode::EngineInit,
            format!("libghostty-vt engine init failed: {error}"),
        )
    })?;

    let engine = pipe.engine();

    engine
        .lock()
        .set_default_cursor_shape(cursor_shape)
        .map_err(|error| {
            EngineError::new(
                EngineErrorCode::EngineInit,
                format!("failed to configure cursor shape: {error}"),
            )
        })?;

    let messenger = pipe.channel();

    drop(pipe.spawn());

    Ok((engine, messenger))
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
    frozen_images: crate::terminal::graphics::FrozenImageCache,
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
        let batch = std::mem::take(&mut *self.staged_blocks.lock());
        if batch.is_empty() {
            return;
        }
        crate::terminal::graphics::prune_frozen_images(&self.frozen_images, &batch);
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
            TerminalEvent::ClipboardStore(ty, text) => {
                let mut clipboard = nmt_terminal::clipboard::Clipboard::default();

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
            TerminalEvent::ReportToAssistant(err) => HostEvent::Diagnostic(err.report.to_string()),
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

                tracing::debug!(
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
    pub cursor_shape: nmt_config::CursorShape,
    /// Scrollback budget in lines; converted to the engine's byte budget.
    pub scrollback_lines: usize,
    /// Engine-blocks mode is the default because completed commands can freeze
    /// into engine-side blocks at each trusted `;D`; rendering reads
    /// them through `BlockRef` handles. `false` is the internal classic-grid
    /// fallback: no freezing, no boundary clears, no block events, intact
    /// scrollback. The GPUI app keeps this enabled and toggles block chrome only.
    pub engine_blocks: bool,
    /// Route this new terminal through SessionHub instead of creating ConPTY locally.
    pub remote_session_enabled: bool,
    /// Child-only values merged into the shell's inherited Windows environment.
    /// Runtime metadata is deliberately excluded from persisted tab state.
    pub environment_overrides: Vec<(String, String)>,
}

impl TerminalSessionConfig {
    pub(crate) fn restorable_tab_state(&self) -> nmt_config::local_state::TabState {
        nmt_config::local_state::TabState {
            name: None,
            user_named: false,
            shell: self.shell.clone(),
            args: self.args.clone(),
            cwd: self.working_dir.clone(),
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

    base64::engine::general_purpose::STANDARD.encode(bytes)
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
            cursor_shape: nmt_config::CursorShape::Block,
            scrollback_lines: 10_000,
            engine_blocks: true,
            remote_session_enabled: false,
            environment_overrides: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nmt_terminal::event::TerminalEvent;
    use parking_lot::Mutex;

    use crate::error::EngineErrorCode;
    use crate::terminal::session::{
        HostEvent, HostEventQueue, SessionGraphics, TerminalEventProxy, TerminalSession,
        TerminalSessionConfig,
    };
    use crate::terminal::wake::Wake;
    use crate::utils::POWERSHELL_INTEGRATION;

    #[test]
    fn trusted_prompt_integration_requires_injected_powershell_startup() {
        assert!(TerminalSessionConfig::default().has_trusted_prompt_integration());

        assert!(
            TerminalSessionConfig {
                shell: Some("pwsh.exe".into()),
                ..TerminalSessionConfig::default()
            }
            .has_trusted_prompt_integration()
        );

        assert!(
            !TerminalSessionConfig {
                shell: Some("pwsh.exe".into()),
                args: vec!["-NoLogo".into()],
                ..TerminalSessionConfig::default()
            }
            .has_trusted_prompt_integration()
        );

        assert!(
            !TerminalSessionConfig {
                shell: Some("cmd.exe".into()),
                ..TerminalSessionConfig::default()
            }
            .has_trusted_prompt_integration()
        );
    }

    #[test]
    fn powershell_bootstrap_is_passed_as_utf16_encoded_command() {
        use base64::Engine as _;

        let config = TerminalSessionConfig::default().with_shell_integration();
        let encoded = &config.args[2];
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let utf16: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();

        assert_eq!(config.args[0], "-NoExit");
        assert_eq!(config.args[1], "-EncodedCommand");
        assert_eq!(
            String::from_utf16(utf16.as_slice()).unwrap(),
            POWERSHELL_INTEGRATION
        );
    }

    /// Creating a session with a non-existent shell returns a structured
    /// `PtySpawn` error rather than a bare null so callers retain the failure cause.
    #[test]
    fn bad_shell_returns_structured_error() {
        let config = TerminalSessionConfig {
            shell: Some("this-shell-does-not-exist-xyz.exe".into()),
            ..TerminalSessionConfig::default()
        };
        let err = TerminalSession::new_internal(&config, 1, None)
            .err()
            .expect("a non-existent shell must fail");
        assert_eq!(err.code, EngineErrorCode::PtySpawn);
        assert!(!err.message.is_empty());
    }

    #[test]
    fn restorable_tab_state_keeps_original_launch_command() {
        let config = TerminalSessionConfig {
            shell: Some("pwsh.exe".to_string()),
            working_dir: Some("C:/Projects/example".to_string()),
            ..TerminalSessionConfig::default()
        };

        let state = config.restorable_tab_state();
        let integrated = config.with_shell_integration();

        assert_eq!(state.shell.as_deref(), Some("pwsh.exe"));
        assert!(state.args.is_empty());
        assert_eq!(state.cwd.as_deref(), Some("C:/Projects/example"));
        assert!(!integrated.args.is_empty());
    }

    /// `NiumaTermEventListener` maps user-visible `TerminalEvent`s onto the host-event queue
    /// the shell drains, including title (incl. reset → empty), bell, exit, and
    /// desktop notification.
    #[test]
    fn host_events_map_from_terminal_events() {
        use nmt_terminal::event::{EventListener, TerminalEvent, WindowId};

        let events: HostEventQueue = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let listener = TerminalEventProxy {
            events: Arc::clone(&events),
            block_store: Arc::new(Mutex::new(nmt_terminal::block_store::BlockStore::default())),
            in_flight: Arc::new(Mutex::new(None)),
            open_prompt: Arc::new(Mutex::new(false)),
            generation_store: Arc::new(Mutex::new(
                crate::terminal::graphics::GenerationStore::new(),
            )),
            staged_blocks: Arc::new(Mutex::new(Vec::new())),
            frozen_images: Default::default(),
            live_image_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            id: 1,
            wake: None,
        };
        let wid = WindowId::dummy();

        listener.send_event(TerminalEvent::Title("t".into()), wid);
        listener.send_event(TerminalEvent::ResetTitle, wid);
        listener.send_event(TerminalEvent::Bell, wid);
        listener.send_event(TerminalEvent::CloseTerminal(0), wid);
        listener.send_event(
            TerminalEvent::DesktopNotification {
                title: "T".into(),
                body: "B".into(),
            },
            wid,
        );
        listener.send_event(TerminalEvent::PromptBoundaryTrusted(true), wid);

        let q = events.lock();
        let v: Vec<&HostEvent> = q.iter().collect();
        assert!(matches!(v[0], HostEvent::Title(s) if s == "t"));
        assert!(matches!(v[1], HostEvent::Title(s) if s.is_empty()));
        assert!(matches!(v[2], HostEvent::Bell));
        assert!(matches!(v[3], HostEvent::Exit));
        assert!(
            matches!(v[4], HostEvent::Notification { title, body } if title == "T" && body == "B")
        );
        assert!(matches!(v[5], HostEvent::PromptBoundaryTrusted(true)));
    }

    #[test]
    fn osc_notification_drains_into_shared_exact_notification_lifecycle() {
        use std::time::Instant;

        use nmt_agent_utils::{
            AgentMonitor, AgentRoute, AgentRuntimeStatus, request_native_delivery,
        };
        use nmt_terminal::event::{EventListener, TerminalEvent, WindowId};

        let events: HostEventQueue = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let listener = TerminalEventProxy {
            events: Arc::clone(&events),
            block_store: Arc::new(Mutex::new(nmt_terminal::block_store::BlockStore::default())),
            in_flight: Arc::new(Mutex::new(None)),
            open_prompt: Arc::new(Mutex::new(false)),
            generation_store: Arc::new(Mutex::new(
                crate::terminal::graphics::GenerationStore::new(),
            )),
            staged_blocks: Arc::new(Mutex::new(Vec::new())),
            frozen_images: Default::default(),
            live_image_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            id: 1,
            wake: None,
        };
        listener.send_event(
            TerminalEvent::DesktopNotification {
                title: "T".repeat(300),
                body: "B".repeat(5_000),
            },
            WindowId::dummy(),
        );

        let route = AgentRoute::parse("osc-route").unwrap();
        let mut monitor = AgentMonitor::new("process");
        monitor.register_route(route.clone(), Instant::now());
        let event = events.lock().pop_front().unwrap();
        let HostEvent::Notification { title, body } = event else {
            panic!("expected OSC notification host event");
        };
        monitor.notify(&route, &title, &body);

        let notification = monitor.notification(&route).unwrap().clone();
        assert_eq!(notification.title.chars().count(), 256);
        assert_eq!(notification.body.chars().count(), 4_096);
        assert_eq!(monitor.project([&route]).status, AgentRuntimeStatus::Idle);
        assert!(request_native_delivery(None, &route));
        assert!(monitor.mark_native_requested(&route, &notification.id));
        assert!(
            monitor
                .acknowledge(&route, &notification.id)
                .visible_changed
        );
        assert_eq!(monitor.project([&route]).unread_count, 0);
    }

    /// In-flight lifecycle: CommandStarted sets the running block, CommandFinished
    /// finalizes it in place; trust loss and exit clear it without appending a block.
    #[test]
    fn in_flight_block_lifecycle() {
        use std::time::SystemTime;

        use nmt_terminal::event::{
            CommandCapture, CommandStart, EventListener, TerminalEvent, WindowId,
        };

        fn start(cmd: &str) -> CommandStart {
            CommandStart {
                seq: 0,
                command: cmd.to_string(),
                cwd: Some("C:/w".into()),
                started_at: SystemTime::now(),
            }
        }
        fn capture(cmd: &str) -> CommandCapture {
            let now = SystemTime::now();
            CommandCapture {
                seq: 0,
                command: cmd.to_string(),
                exit_code: Some(0),
                cwd: Some("C:/w".into()),
                started_at: now,
                ended_at: now,
            }
        }

        let events: HostEventQueue = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let in_flight = Arc::new(Mutex::new(None));
        let open_prompt = Arc::new(Mutex::new(false));
        let proxy = TerminalEventProxy {
            events: Arc::clone(&events),
            block_store: Arc::new(Mutex::new(nmt_terminal::block_store::BlockStore::default())),
            in_flight: Arc::clone(&in_flight),
            open_prompt: Arc::clone(&open_prompt),
            generation_store: Arc::new(Mutex::new(
                crate::terminal::graphics::GenerationStore::new(),
            )),
            staged_blocks: Arc::new(Mutex::new(Vec::new())),
            frozen_images: Default::default(),
            live_image_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            id: 1,
            wake: None,
        };
        let wid = WindowId::dummy();

        proxy.send_event(TerminalEvent::PromptStarted, wid);
        assert!(*open_prompt.lock());

        // start -> finish: in-flight visible while running, then cleared.
        proxy.send_event(TerminalEvent::CommandStarted(start("sleep 5")), wid);
        assert!(!*open_prompt.lock(), "command start closes prompt");
        {
            let running = in_flight.lock().clone().expect("in-flight set");
            assert_eq!(running.command.as_str(), "sleep 5");
        }
        proxy.send_event(TerminalEvent::CommandFinished(capture("sleep 5")), wid);
        assert!(
            in_flight.lock().is_none(),
            "finished command clears live state"
        );

        // start -> trust loss: cleared.
        proxy.send_event(TerminalEvent::CommandStarted(start("nested")), wid);
        assert_eq!(in_flight.lock().clone().unwrap().command, "nested");
        proxy.send_event(TerminalEvent::PromptStarted, wid);
        assert!(*open_prompt.lock());
        proxy.send_event(TerminalEvent::PromptBoundaryTrusted(false), wid);
        assert!(
            in_flight.lock().is_none(),
            "trust loss drops the running block"
        );
        assert!(!*open_prompt.lock(), "trust loss closes prompt");

        // start -> exit: cleared as well.
        proxy.send_event(TerminalEvent::CommandStarted(start("hang")), wid);
        proxy.send_event(TerminalEvent::PromptStarted, wid);
        assert!(*open_prompt.lock());
        proxy.send_event(TerminalEvent::CloseTerminal(0), wid);
        assert!(in_flight.lock().is_none(), "exit drops the running block");
        assert!(!*open_prompt.lock(), "exit closes prompt");

        // The host queue saw the prompt and command events too, in order.
        let q = events.lock();
        assert!(matches!(&q[0], HostEvent::PromptStarted));
        assert!(matches!(&q[1], HostEvent::CommandStarted));
        assert!(matches!(&q[2], HostEvent::CommandFinished));
    }

    /// Block-split wiring: `BlockBatch` events feed the shared store, and
    /// `CommandStarted`/`CommandFinished` metadata marries its segment by
    /// `seq` even though the marks fire long before the segment's lines
    /// scroll out of the active area.
    #[test]
    fn block_batches_and_seq_metadata_reach_the_block_store() {
        use std::time::SystemTime;

        use nmt_terminal::event::{
            BlockEvent, CommandCapture, CommandStart, EventListener, TerminalEvent, WindowId,
        };
        use nmt_terminal::ghostty::BlockHandle;

        let store = Arc::new(Mutex::new(nmt_terminal::block_store::BlockStore::default()));
        let proxy = TerminalEventProxy {
            events: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            block_store: Arc::clone(&store),
            in_flight: Arc::new(Mutex::new(None)),
            open_prompt: Arc::new(Mutex::new(false)),
            generation_store: Arc::new(Mutex::new(
                crate::terminal::graphics::GenerationStore::new(),
            )),
            staged_blocks: Arc::new(Mutex::new(Vec::new())),
            frozen_images: Default::default(),
            live_image_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            id: 1,
            wake: None,
        };
        let wid = WindowId::dummy();
        let now = SystemTime::now();

        // Marks fire first (write time)...
        proxy.send_event(
            TerminalEvent::CommandStarted(CommandStart {
                seq: 1,
                command: "cargo build".into(),
                cwd: Some("C:/w".into()),
                started_at: now,
            }),
            wid,
        );
        proxy.send_event(
            TerminalEvent::CommandFinished(CommandCapture {
                seq: 1,
                command: "cargo build".into(),
                exit_code: Some(0),
                cwd: Some("C:/w".into()),
                started_at: now,
                ended_at: now,
            }),
            wid,
        );
        // ...the item materializes later, at the block's finish. The batch is
        // staged and only flushed to the store on the read's damage wake, so
        // nothing lands until the following `TerminalDamaged`.
        proxy.send_event(
            TerminalEvent::BlockBatch(vec![BlockEvent::EngineBlock {
                seq: 1,
                handle: BlockHandle {
                    id: 1,
                    generation: 1,
                },
                rows: 3,
            }]),
            wid,
        );
        assert!(
            store.lock().items().is_empty(),
            "staged batch must not reach the store before the damage flush"
        );
        proxy.send_event(TerminalEvent::TerminalDamaged(1), wid);

        let store = store.lock();
        let items = store.items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].seq, Some(1));
        assert_eq!(items[0].engine_rows(), 3);
        assert_eq!(items[0].meta.command.as_deref(), Some("cargo build"));
        assert_eq!(items[0].meta.exit_code, Some(0));
    }

    /// Build a `TerminalEventProxy` whose state Arcs the test retains, plus a wake
    /// collector. `id` is the route so `UpdateGraphics` routing can be exercised.
    fn graphics_proxy(id: u64) -> (TerminalEventProxy, GraphicsProbes) {
        use crate::terminal::graphics::GenerationStore;
        use crate::terminal::wake::WakeSender;

        let events: HostEventQueue = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let block_store = Arc::new(Mutex::new(nmt_terminal::block_store::BlockStore::default()));
        let generation_store = Arc::new(Mutex::new(GenerationStore::new()));
        let staged_blocks = Arc::new(Mutex::new(Vec::new()));
        let wakes = Arc::new(Mutex::new(Vec::new()));
        let wakes_for_sender = Arc::clone(&wakes);
        let proxy = TerminalEventProxy {
            events: Arc::clone(&events),
            block_store: Arc::clone(&block_store),
            generation_store: Arc::clone(&generation_store),
            staged_blocks: Arc::clone(&staged_blocks),
            frozen_images: Default::default(),
            live_image_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            in_flight: Arc::new(Mutex::new(None)),
            open_prompt: Arc::new(Mutex::new(false)),
            id,
            wake: Some(WakeSender::from_fn(move |w| {
                wakes_for_sender.lock().push(w)
            })),
        };
        (
            proxy,
            GraphicsProbes {
                events,
                block_store,
                generation_store,
                staged_blocks,
                wakes,
            },
        )
    }

    struct GraphicsProbes {
        events: HostEventQueue,
        block_store: Arc<Mutex<nmt_terminal::block_store::BlockStore>>,
        generation_store: SessionGraphics,
        staged_blocks: Arc<Mutex<Vec<nmt_terminal::event::BlockEvent>>>,
        wakes: Arc<Mutex<Vec<Wake>>>,
    }

    fn rgba_update(route_id: usize, image_id: u32, w: usize, h: usize) -> TerminalEvent {
        use nmt_terminal::ansi::graphics::UpdateQueues;
        use nmt_terminal::graphics::{ColorType, GraphicData, GraphicId};
        let data = GraphicData {
            id: GraphicId(image_id as u64),
            width: w,
            height: h,
            color_type: ColorType::Rgba,
            pixels: vec![0u8; w * h * 4],
            is_opaque: true,
            resize: None,
            display_width: None,
            display_height: None,
            transmit_time: std::time::Instant::now(),
        };
        TerminalEvent::UpdateGraphics {
            route_id,
            queues: UpdateQueues {
                pending: Vec::new(),
                pending_images: vec![(image_id, data)],
                remove_queue: Vec::new(),
            },
        }
    }

    /// `UpdateGraphics` installs a live generation and wakes for content,
    /// but never enqueues a host event; a mismatched route is ignored entirely.
    #[test]
    fn graphics_events_bypass_host_queue_and_are_route_scoped() {
        use nmt_terminal::event::{EventListener, WindowId};
        let (proxy, p) = graphics_proxy(4);
        let wid = WindowId::dummy();

        proxy.send_event(rgba_update(4, 7, 2, 2), wid);
        assert!(
            p.events.lock().is_empty(),
            "graphics never enters the host queue"
        );
        assert!(
            p.generation_store.lock().get(7).is_some(),
            "generation installed"
        );
        assert_eq!(*p.wakes.lock(), vec![Wake::Content(4)], "one content wake");

        // A cross-session route is dropped: no install, no wake.
        proxy.send_event(rgba_update(999, 8, 2, 2), wid);
        assert!(
            p.generation_store.lock().get(8).is_none(),
            "wrong route ignored"
        );
        assert_eq!(p.wakes.lock().len(), 1, "no wake for wrong route");
    }

    /// Sustained output cannot grow an unbounded UI-facing queue. Each
    /// read's staged block events flush on their damage wake, and the host-event queue is
    /// never touched, so neither the staging buffer nor the host queue accumulates.
    #[test]
    fn sustained_output_does_not_grow_ui_queue() {
        use nmt_terminal::event::{BlockEvent, EventListener, WindowId};
        use nmt_terminal::ghostty::BlockHandle;
        let (proxy, p) = graphics_proxy(1);
        let wid = WindowId::dummy();

        for seq in 0..1000u64 {
            proxy.send_event(
                TerminalEvent::BlockBatch(vec![BlockEvent::EngineBlocksSync(vec![(
                    BlockHandle {
                        id: seq,
                        generation: 1,
                    },
                    1,
                )])]),
                wid,
            );
            proxy.send_event(rgba_update(1, 1, 1, 1), wid);
            proxy.send_event(TerminalEvent::TerminalDamaged(1), wid);
            // After each read's damage flush the staging buffer is empty again.
            assert!(
                p.staged_blocks.lock().is_empty(),
                "staging bounded to one read"
            );
        }
        assert!(
            p.events.lock().is_empty(),
            "host queue never grew from graphics/block events"
        );
        // The live generation is a single replaced entry, not 1000 accumulated ones.
        assert_eq!(
            p.generation_store.lock().len(),
            1,
            "one live generation, replaced"
        );
    }

    /// On the UI wake, active (live generation) and frozen (block-store
    /// history) image state are both present — the read installed the generation
    /// before flushing the block batch that froze the same content.
    #[test]
    fn active_and_frozen_state_coherent_at_wake() {
        use nmt_terminal::event::{BlockEvent, EventListener, WindowId};
        use nmt_terminal::ghostty::BlockHandle;
        let (proxy, p) = graphics_proxy(1);
        let wid = WindowId::dummy();

        // Order within a read: block event staged, then generation installed,
        // then damage.
        proxy.send_event(
            TerminalEvent::BlockBatch(vec![BlockEvent::EngineBlock {
                seq: 1,
                handle: BlockHandle {
                    id: 1,
                    generation: 1,
                },
                rows: 2,
            }]),
            wid,
        );
        proxy.send_event(rgba_update(1, 42, 2, 2), wid);
        // Before the flush the frozen row is not yet in the store.
        assert!(p.block_store.lock().items().is_empty());
        proxy.send_event(TerminalEvent::TerminalDamaged(1), wid);

        // At the wake both sides are coherent: live generation present AND frozen row
        // committed to history.
        assert!(
            p.generation_store.lock().get(42).is_some(),
            "live generation present"
        );
        assert_eq!(
            p.block_store.lock().items().len(),
            1,
            "frozen history committed by the same wake"
        );
        assert!(p.wakes.lock().contains(&Wake::Content(1)));
    }
}
