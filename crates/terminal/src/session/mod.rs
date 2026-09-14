//! One terminal surface's runtime state: libghostty-vt engine, render buffer, and
//! the ConPTY-backed PTY worker so platform details stay outside the UI layer.

pub use crate::session::blocks::BlockPoint;
pub use crate::session::config::TerminalSessionConfig;
pub use crate::session::error::{EngineError, EngineErrorCode};
pub use crate::session::mouse::{
    SurfaceCell, SurfaceCellSide, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
};
pub use crate::session::observer::{SessionChange, SessionObserver};
pub use crate::session::rows::RowText;

pub mod page;
pub mod request;

pub mod interaction;

pub(crate) mod selection;

mod blocks;

mod rows;

mod config;
mod error;

mod mouse;
mod observer;
mod proxy;

#[cfg(test)]
mod interaction_tests;
#[cfg(test)]
mod psreadline_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vtebench_tests;

#[cfg(test)]
mod block_tests;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::{io, time};

use futures::channel::oneshot;
use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_input::event::ElementState;
use nmt_input::keyboard::{Key, KeyLocation, ModifiersState};
use nmt_input::{
    KeyEncodeFlags, KeyInput, bracket_paste, encode_mouse_report, encode_terminal_input,
};
use nmt_platform::process::ProcessTree;
use nmt_platform::{
    EventedPty, PtyOptions, WinsizeBuilder, create_managed_pty_with_env, create_pty_with_env,
};
use parking_lot::Mutex;
use tracing::error;

use crate::block_store::{BlockItem, BlockStore};
use crate::event::{Msg, MsgSender, ProgressReport};
use crate::ghostty::BlockHandle;
use crate::graphics::GraphicData;
use crate::input::{TerminalKey, key_encode_flags, should_defer_to_ime};
use crate::pty_pipe::{SessionOptions, SessionWorker, start_session};
use crate::publication::FrameStore;
use crate::render_buffer::RenderBuffer;
use crate::selection::{SelectionRange, SelectionType, WORD_DELIMITERS};
use crate::session::blocks::frozen_selection_pieces;
use crate::session::config::{default_shell, is_windows_powershell};
use crate::session::mouse::{mouse_button_code, mouse_motion_code, mouse_report_mods};
use crate::session::page::{PageCache, PageSource, RowPage};
use crate::session::proxy::TerminalEventProxy;
use crate::session::request::{BlockRange, Query, Request, TextPiece, TextSource};
use crate::session::rows::materialized_pointer_row;
use crate::session::selection::{SurfaceSelection, selection_screen_range};
use crate::terminal::Mode;
use crate::terminal::pos::{Column, Line, Pos};

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
    pub command: Option<String>,
    pub started_at: time::SystemTime,
}

/// The host's session handle: commands, immutable frame publications, and
/// asynchronous reads. The PTY event loop exclusively owns the engine.
pub struct TerminalSession {
    _worker: SessionWorker,
    pages: RefCell<PageCache>,
    render_buffer: SessionBuffer,
    vt_modes: Arc<AtomicU32>,
    messenger: MsgSender,
    shared: Arc<SessionSharedState>,
    process_tree: Option<ProcessTree>,

    /// Engine-blocks mode is active: frozen history lives in
    /// finished engine blocks, read through owned pages. Mirrors the flag the
    /// PTY event loop runs with.
    engine_blocks: bool,

    supports_powershell_compatibility: bool,
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

    open_prompt: AtomicBool,
    #[cfg(any(test, feature = "test-support"))]
    read_only: AtomicBool,
    exited: AtomicBool,
    alt_screen: AtomicBool,
    selection: SurfaceSelection,
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

        let mut session = Self::from_pty(
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
        )?;

        session.supports_powershell_compatibility = is_windows_powershell(&shell);

        session.set_powershell_compatibility(config.improve_powershell_compatibility);

        Ok(session)
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

        let handles = start_session(pty, proxy, options).map_err(|error| {
            error!("session start failed: {error:?}");

            EngineError::new(
                EngineErrorCode::EngineInit,
                format!("libghostty-vt engine init failed: {error}"),
            )
        })?;

        Ok(Self {
            _worker: handles.worker,
            pages: RefCell::new(PageCache::default()),
            render_buffer: handles.render_buffer,
            vt_modes: handles.vt_modes,
            messenger: handles.messenger,
            shared,
            process_tree,
            engine_blocks,
            supports_powershell_compatibility: false,
        })
    }

    /// Whether frozen history lives in finished engine blocks.
    pub fn engine_blocks(&self) -> bool {
        self.engine_blocks
    }

    /// Remote sessions and other launch executables do not opt into the local
    /// PowerShell workaround, even when the application setting is enabled.
    pub fn set_powershell_compatibility(&self, enabled: bool) -> bool {
        self.supports_powershell_compatibility
            && self
                .messenger
                .send(Msg::PowerShellCompatibility(enabled))
                .is_ok()
    }

    /// Shared frozen block-split history (renderer read side).
    pub fn block_store(&self) -> Arc<Mutex<BlockStore>> {
        Arc::clone(&self.shared.block_store)
    }

    /// Number of processes beyond the shell itself; zero without process
    /// management. Query failures remain distinguishable from an empty group.
    pub fn child_process_count(&self) -> io::Result<usize> {
        self.process_tree.as_ref().map_or(Ok(0), |tree| {
            tree.process_count().map(|count| count.saturating_sub(1))
        })
    }

    /// Write input bytes (already terminal-encoded) to the session's PTY.
    /// True means the local queue accepted the bytes, not that a remote peer
    /// received them or the shell processed them.
    pub fn write_input(&self, data: &[u8]) -> bool {
        if data.is_empty() || self.shared.exited.load(Ordering::Acquire) {
            return false;
        }

        #[cfg(any(test, feature = "test-support"))]
        if self.shared.read_only.load(Ordering::Acquire) {
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn mark_read_only(&self) {
        self.shared.read_only.store(true, Ordering::Release);
    }

    pub fn current_directory(&self) -> Option<String> {
        self.render_buffer
            .load()
            .current_directory()
            .map(str::to_string)
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
        self.shared.open_prompt.load(Ordering::Acquire)
    }

    pub fn block_selection_text(
        &self,
        handle: BlockHandle,
        line: usize,
        col: u32,
        kind: SelectionType,
    ) -> Request<String> {
        self.request_text(TextSource::BlockSelection {
            handle,
            line,
            col,
            kind,
        })
    }

    pub fn block_item(&self, item: usize) -> Option<BlockItem> {
        self.shared.block_store.lock().items().get(item).cloned()
    }

    pub fn block_command(&self, item: usize) -> Option<String> {
        let store = self.shared.block_store.lock();

        if let Some(item) = store.items().get(item) {
            return item.meta.command.clone();
        }

        let live = item == store.items().len();

        drop(store);

        live.then(|| self.in_flight_block())
            .flatten()
            .and_then(|block| block.command)
    }

    pub fn block_text(&self, item: usize) -> Option<Request<String>> {
        let handle = self.block_handle(item)?;

        Some(self.request_text(TextSource::Blocks(vec![TextPiece {
            handle,
            start: None,
            end: None,
        }])))
    }

    pub fn expand_frozen_selection(
        &self,
        at: BlockPoint,
        kind: SelectionType,
    ) -> Option<Request<BlockRange>> {
        let handle = self.block_handle(at.item)?;
        let (reply, request) = oneshot::channel();

        let _ = self.messenger.send(Msg::Query(Query::ExpandSelection {
            handle,
            line: at.line,
            col: at.col,
            kind,
            reply,
        }));

        Some(request)
    }

    pub fn frozen_selection_text(&self, a: BlockPoint, b: BlockPoint) -> Request<String> {
        let pieces = frozen_selection_pieces(&self.shared.block_store.lock(), a, b);

        self.request_text(TextSource::Blocks(
            pieces
                .into_iter()
                .map(|piece| TextPiece {
                    handle: piece.handle,
                    start: piece.start,
                    end: piece.end,
                })
                .collect(),
        ))
    }

    fn request_text(&self, source: TextSource) -> Request<String> {
        let (reply, request) = oneshot::channel();

        let _ = self
            .messenger
            .send(Msg::Query(Query::Text { source, reply }));

        request
    }

    fn block_handle(&self, item: usize) -> Option<BlockHandle> {
        self.shared.block_store.lock().items().get(item)?.handle()
    }

    pub fn paste_paths(&self, paths: &[PathBuf]) -> bool {
        let text = paths
            .iter()
            .map(|path| {
                let path = path.to_string_lossy();

                if path.contains(' ') {
                    format!("\"{path}\"")
                } else {
                    path.into_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        self.paste_text(&text)
    }

    pub fn rerun_block(&self, item: usize) -> bool {
        self.block_command(item)
            .is_some_and(|command| self.write_text(&format!("{command}\r")))
    }

    pub fn write_text(&self, text: &str) -> bool {
        self.write_input(text.as_bytes())
    }

    pub fn defer_key_to_ime(&self, key: &TerminalKey<'_>) -> bool {
        !self.modes().contains(Mode::REPORT_ALL_KEYS_AS_ESC) && should_defer_to_ime(key)
    }

    pub fn commit_text(&self, text: &str) -> bool {
        let flags = key_encode_flags(self.modes());

        if !flags.contains(KeyEncodeFlags::REPORT_ALL_KEYS_AS_ESC) {
            return self.write_text(text);
        }

        // An IME commit has no physical key identity. The empty key makes the
        // encoder use key number zero and include only the requested text.
        let input = KeyInput {
            logical_key: Key::Character("".into()),
            key_without_modifiers: Key::Character("".into()),
            text_with_all_modifiers: Some(text.into()),
            location: KeyLocation::Standard,
            state: ElementState::Pressed,
            repeat: false,
        };

        encode_terminal_input(&input, ModifiersState::empty(), flags, None)
            .is_some_and(|bytes| self.write_input(&bytes))
    }

    pub fn paste_text(&self, text: &str) -> bool {
        let Some(bytes) = paste_payload(text, self.modes().contains(Mode::BRACKETED_PASTE)) else {
            return false;
        };

        self.write_input(&bytes)
    }

    pub fn apply_mouse(
        &self,
        cell: SurfaceCell,
        side: SurfaceCellSide,
        button: Option<SurfaceMouseButton>,
        kind: SurfaceMouseEventKind,
        modifiers: ModifiersState,
        selection_type: SelectionType,
    ) -> bool {
        if let Some(mode) = self.app_mouse_mode(modifiers) {
            return match kind {
                SurfaceMouseEventKind::Down | SurfaceMouseEventKind::Up => {
                    let Some(code) = button.and_then(mouse_button_code) else {
                        return false;
                    };

                    self.report_mouse(
                        mode,
                        code,
                        kind == SurfaceMouseEventKind::Down,
                        cell.col,
                        cell.row,
                        modifiers,
                    )
                }
                SurfaceMouseEventKind::Move => {
                    let Some(code) = mouse_motion_code(mode, button) else {
                        return false;
                    };

                    self.report_mouse(mode, code, true, cell.col, cell.row, modifiers)
                }
            };
        }

        if button != Some(SurfaceMouseButton::Left) {
            return false;
        }

        let viewport_top = self
            .snapshot()
            .viewport_top()
            .unwrap_or(0)
            .min(i32::MAX as u32) as i32;

        let pos = Pos::new(
            Line((cell.row as i32).saturating_add(viewport_top)),
            Column(cell.col as usize),
        );

        self.shared
            .selection
            .apply_at(pos, side, kind, selection_type)
    }

    fn modes(&self) -> Mode {
        let bits = self.vt_modes.load(Ordering::Relaxed);

        Mode::from_bits_truncate(bits)
    }

    fn mouse_mode(&self) -> Option<Mode> {
        let mode = self.modes();

        mode.intersects(Mode::MOUSE_MODE).then_some(mode)
    }

    fn app_mouse_mode(&self, modifiers: ModifiersState) -> Option<Mode> {
        if modifiers.shift_key() {
            return None;
        }

        self.mouse_mode()
    }

    fn report_mouse(
        &self,
        mode: Mode,
        button: u8,
        pressed: bool,
        col: u16,
        row: u16,
        modifiers: ModifiersState,
    ) -> bool {
        let Some(msg) = encode_mouse_report(
            mode.contains(Mode::SGR_MOUSE),
            button,
            mouse_report_mods(modifiers),
            pressed,
            col,
            row,
        ) else {
            return false;
        };

        self.write_input(&msg)
    }

    pub fn title(&self) -> String {
        self.snapshot().title().to_string()
    }

    /// Reports queue acceptance; the owner publishes colors with its next frame.
    pub fn set_theme_colors(&self, colors: &Colors) -> bool {
        self.messenger.send(Msg::Theme(Box::new(*colors))).is_ok()
    }

    pub fn set_cursor_shape(&self, shape: CursorShape) -> Request<()> {
        let (reply, request) = oneshot::channel();
        let _ = self.messenger.send(Msg::CursorShape { shape, reply });

        request
    }

    pub fn snapshot(&self) -> Arc<RenderBuffer> {
        self.render_buffer.load()
    }

    pub fn with_render_buffer<R>(&self, read: impl FnOnce(&RenderBuffer) -> R) -> R {
        read(&self.snapshot())
    }

    pub fn mouse_reporting_active(&self) -> bool {
        self.mouse_mode().is_some()
    }

    pub fn mouse_reporting_active_for(&self, modifiers: ModifiersState) -> bool {
        self.app_mouse_mode(modifiers).is_some()
    }

    pub fn take_block_image(&self, handle: BlockHandle, image_id: u32) -> Option<GraphicData> {
        self.pages
            .borrow_mut()
            .take_image(handle, image_id, &self.messenger)
    }

    pub fn screen_page_at(&self, revision: u64, row: usize) -> Option<Arc<RowPage>> {
        self.pages
            .borrow_mut()
            .read(PageSource::Screen { revision }, row, &self.messenger)
    }

    pub fn block_page(&self, handle: BlockHandle, row: usize) -> Option<Arc<RowPage>> {
        let source = PageSource::Block {
            id: handle.id,
            generation: handle.generation,
            theme: self.snapshot().theme_revision(),
        };

        self.pages.borrow_mut().read(source, row, &self.messenger)
    }

    pub fn screen_row_text_in(&self, snapshot: &RenderBuffer, row: u32) -> Option<RowText> {
        if let Some(index) = snapshot.viewport_top().and_then(|top| row.checked_sub(top))
            && let Some(cells) = snapshot.grid().get(index as usize)
        {
            return Some(RowText {
                text: cells
                    .inner
                    .iter()
                    .map(|cell| match cell.c() {
                        '\0' => ' ',
                        c => c,
                    })
                    .collect(),
                wrapped: snapshot.row_wrapped(index as usize),
                hyperlinks: snapshot.row_hyperlinks(index as usize).to_vec(),
            });
        }

        let page = self.screen_page_at(snapshot.revision(), row as usize)?;

        Some(materialized_pointer_row(page.row(row as usize)?, page.cols))
    }

    pub fn block_row_text(&self, item: usize, row: usize) -> Option<RowText> {
        let page = self.block_page(self.block_handle(item)?, row)?;

        Some(materialized_pointer_row(page.row(row)?, page.cols))
    }

    pub fn scroll_to(&self, offset: u64) -> bool {
        !self.exited() && self.messenger.send(Msg::ScrollTo(offset)).is_ok()
    }

    pub fn scroll_to_end(&self) -> bool {
        !self.exited() && self.messenger.send(Msg::ScrollToEnd).is_ok()
    }

    pub fn apply_scroll(&self, cell: SurfaceCell, lines: i32, modifiers: ModifiersState) -> bool {
        if lines == 0 {
            return false;
        }

        if let Some(mode) = self.mouse_mode() {
            let button = if lines > 0 { 64 } else { 65 };

            return self.report_mouse(mode, button, true, cell.col, cell.row, modifiers);
        }

        if self.exited() {
            return false;
        }

        let before = self.snapshot().scrollbar();

        if before.total <= before.len {
            return false;
        }

        // Queue acceptance precedes the published position and history-edge clamping.
        self.messenger.send(Msg::Scroll(-(lines as isize))).is_ok()
    }

    pub fn selected_text_in(&self, snapshot: &RenderBuffer) -> Option<Request<String>> {
        let selection = self.shared.selection.selection.lock();

        let range = selection_screen_range(
            selection.as_ref()?,
            snapshot,
            snapshot.viewport_top().unwrap_or(0) as i32,
        )?;

        let start = (
            u16::try_from(range.start.col.0).ok()?,
            u32::try_from(range.start.row.0).ok()?,
        );

        let end = (
            u16::try_from(range.end.col.0).ok()?,
            u32::try_from(range.end.row.0).ok()?,
        );

        Some(self.request_text(TextSource::Screen {
            revision: snapshot.revision(),
            start,
            end,
            rectangle: range.is_block,
        }))
    }

    pub fn selection_range_in(&self, snapshot: &RenderBuffer) -> Option<SelectionRange> {
        let selection = self.shared.selection.selection.lock();

        selection.as_ref()?.to_range_engine(
            snapshot,
            snapshot.viewport_top().unwrap_or(0) as i32,
            WORD_DELIMITERS,
        )
    }

    pub fn apply_screen_selection(
        &self,
        cell: SurfaceScreenCell,
        side: SurfaceCellSide,
        kind: SurfaceMouseEventKind,
        selection_type: SelectionType,
    ) -> bool {
        self.shared
            .selection
            .apply_screen(cell, side, kind, selection_type)
    }

    pub fn selection_screen_range_in(&self, snapshot: &RenderBuffer) -> Option<SelectionRange> {
        let selection = self.shared.selection.selection.lock();

        selection_screen_range(
            selection.as_ref()?,
            snapshot,
            snapshot.viewport_top().unwrap_or(0) as i32,
        )
    }

    pub fn clear_selection(&self) {
        self.shared.selection.clear();
    }
}

fn paste_payload(text: &str, bracketed: bool) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }

    let mut body = text.replace("\r\n", "\r").replace('\n', "\r");

    if bracketed {
        body = body.replace("\x1b[201~", "");
    }

    Some(bracket_paste(body.as_bytes(), bracketed))
}
