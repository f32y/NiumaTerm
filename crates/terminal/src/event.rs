use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use nmt_config::colors::ColorRgb;
use nmt_platform::{Waker, WinsizeBuilder};

use crate::ansi::graphics::UpdateQueues;
use crate::clipboard::ClipboardType;
use crate::error::TerminalError;
use crate::terminal::Match;
use crate::terminal::pos::{Direction, Pos};

/// Opaque window identifier carried on terminal events. In the GPUI shell this
/// is just an id value (the old winit `WindowId` is gone); headless sessions use
/// `dummy()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

impl WindowId {
    pub const fn dummy() -> Self {
        Self(0)
    }
}

impl From<u64> for WindowId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

/// One PTY-thread block event: a trusted
/// `;D` freezes the whole command into a finished engine block
/// (`finish_block`, O(1) ownership transfer) and the app receives the
/// HANDLE; rendering reads the frozen block directly through a refcounted
/// `BlockRef`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockEvent {
    /// The user cleared the terminal (announced in-band via the `;K` mark):
    /// the whole frozen history drops with the screen (the PTY side already
    /// cleared the engine blocks).
    HistoryCleared,
    /// A trusted `;D` froze the command into a finished engine block. The
    /// store keeps only the handle; rendering reads the block through
    /// `BlockRef`. `rows` is the row count at finish time, cached app-side
    /// so layout never takes the engine lock.
    EngineBlock {
        seq: u64,
        handle: crate::ghostty::BlockHandle,
        rows: usize,
    },
    /// The engine's current live block list, oldest first, with per-block
    /// row counts. Emitted after resize (eager reflow bumps generations and
    /// re-wraps rows) and after each finish (budget eviction may have
    /// dropped oldest blocks). The store prunes items whose handle is gone
    /// and refreshes cached rows/generation for the rest.
    EngineBlocksSync(Vec<(crate::ghostty::BlockHandle, usize)>),
}

/// A completed integrated-shell command, captured from the OSC 133 lifecycle by the PTY
/// prompt sniffer. `command` is the `;B`→`;C` echo, control-stripped and trimmed.
/// `exit_code` comes from the `;D;<code>` argument (`None` for a foreign bare `;D`).
/// `cwd` is the **launch** working directory, latched at command start — the ps1 reports
/// the next prompt's OSC 7 just before `;D`, so a `cd` records its origin. Timestamps are
/// wall-clock at output-start (`;C`) and command-finished (`;D`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCapture {
    /// OSC 133 prompt-boundary sequence number of the block's prompt (`;A`),
    /// marrying this metadata to its block item (block-split).
    pub seq: u64,
    pub command: String,
    pub exit_code: Option<i32>,
    pub cwd: Option<std::path::PathBuf>,
    pub started_at: std::time::SystemTime,
    pub ended_at: std::time::SystemTime,
}

/// An integrated-shell command that just began executing (trusted `;C` with a non-empty
/// echo), for the in-flight command block. Carries the same launch metadata as the
/// eventual [`CommandCapture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandStart {
    /// See [`CommandCapture::seq`].
    pub seq: u64,
    pub command: String,
    pub cwd: Option<std::path::PathBuf>,
    pub started_at: std::time::SystemTime,
}

#[derive(Debug, Clone)]
pub enum TerminalEventType {
    Terminal(TerminalEvent),
    Frame,
}

#[derive(Debug)]
pub enum Msg {
    /// Data that should be written to the PTY.
    Input(Cow<'static, [u8]>),

    #[allow(dead_code)]
    Shutdown,

    Resize(WinsizeBuilder),
}

/// A `Msg` sender that wakes the PTY event loop's mio `Poll` after each send, so the
/// loop re-polls and drains the receiver. mio 1.2 has no pollable channel, so the
/// `std::sync::mpsc` channel is paired with the loop's `Waker`.
#[derive(Clone)]
pub struct MsgSender {
    tx: std::sync::mpsc::Sender<Msg>,
    waker: Option<Arc<Waker>>,
}

impl MsgSender {
    pub fn new(tx: std::sync::mpsc::Sender<Msg>, waker: Arc<Waker>) -> Self {
        Self {
            tx,
            waker: Some(waker),
        }
    }

    /// A sender with no live loop behind it (dead/placeholder context). Sends fail
    /// silently (the receiver is already dropped) and never wake anything.
    pub fn disconnected() -> Self {
        let (tx, _rx) = std::sync::mpsc::channel();
        Self { tx, waker: None }
    }

    pub fn send(&self, msg: Msg) -> Result<(), std::sync::mpsc::SendError<Msg>> {
        self.tx.send(msg)?;

        // Wake the loop so it drains the receiver. A failed wake means the loop is gone.
        if let Some(waker) = &self.waker {
            let _ = waker.wake();
        }

        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ClickState {
    None,
    Click,
    DoubleClick,
    TripleClick,
}

/// Terminal damage hint — coarse signal for the renderer's update path.
/// The actual per-row decision lives on the snapshot's `Row::dirty`
/// (post-`snapshot_visible`); this enum just gates `update` itself
/// (skip vs incremental vs full rebuild). Variants:
/// - `Noop` — no terminal-side change worth rendering for
/// - `Full` — global state changed (resize, palette, mode flip),
///   force a full rebuild even if no individual row is dirty
/// - `Partial` — at least one row's content changed; the snapshot's
///   per-row dirty bits identify which rows
/// - `CursorOnly` — cursor moved/blinked, no cell content changed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalDamage {
    /// Nothing changed — skip rendering entirely
    #[default]
    Noop,
    /// The entire terminal needs to be redrawn
    Full,
    /// At least one row changed; consult per-row dirty bits
    Partial,
    /// Only the cursor position has changed
    CursorOnly,
}

#[derive(Clone)]
pub enum TerminalEvent {
    PrepareRender(u64),
    PrepareRenderOnRoute(u64, usize),
    PrepareUpdateConfig,
    /// New terminal content available.
    Render,
    /// New terminal content available per route.
    RenderRoute(usize),
    /// Terminal content changed — lightweight notification (no damage payload).
    /// Damage stays in the terminal; renderer extracts it when it locks.
    TerminalDamaged(usize),
    /// Graphics update available from terminal.
    UpdateGraphics {
        route_id: usize,
        queues: UpdateQueues,
    },
    /// A `q` (query) request arrived from the PTY in `route_id`. The
    /// frontend computes the four-state status — System and/or
    /// Glossary coverage — by consulting both `FontLibrary` (system
    /// fonts) and the per-route glyph registry, then writes the
    /// formatted reply back to the same pane's PTY. Asynchronous
    /// because the dispatcher (in rio-backend) doesn't have access
    /// to the FontLibrary; the frontend does.
    GlyphProtocolQuery {
        route_id: usize,
        cp: u32,
    },
    Paste,
    Copy(String),
    UpdateFontSize(u8),
    ToggleFullScreen,
    ToggleAppearanceTheme,
    Minimize(bool),
    Hide,
    HideOtherApplications,
    UpdateConfig,
    CreateWindow,
    CloseWindow,
    CreateNativeTab(Option<String>),
    CreateConfigEditor,
    SelectNativeTabByIndex(usize),
    SelectNativeTabLast,
    SelectNativeTabNext,
    SelectNativeTabPrev,

    ReportToAssistant(TerminalError),

    /// Grid has changed possibly requiring a mouse cursor shape change.
    MouseCursorDirty,

    /// Window title change.
    Title(String),

    /// Window title change.
    TitleWithSubtitle(String, String),

    /// The surface entered (`true`) or left (`false`) an interactive state:
    /// a full-screen program (alt-screen). Edge-triggered.
    InteractiveState(bool),

    /// A full-screen program entered (`true`) or left (`false`) the alt-screen — a
    /// mirror of [`Self::InteractiveState`]. Edge-triggered. Lets the app suppress
    /// command-block chrome only when a TUI repaints the whole grid.
    AltScreen(bool),

    /// The OSC 133 prompt/command/output lifecycle is currently trusted for
    /// command/prompt block ownership. Edge-triggered by the PTY prompt sniffer.
    PromptBoundaryTrusted(bool),

    /// A trusted OSC 133 prompt-start (`;A`) opened an active prompt region.
    PromptStarted,
    /// A batch of block events from the PTY thread:
    /// finished-block handles and lifecycle changes, in stream order.
    BlockBatch(Vec<BlockEvent>),

    /// An integrated-shell command completed under a trusted OSC 133 lifecycle
    /// (command-blocks). Emitted by the PTY prompt sniffer at the command-finished
    /// (`;D`) mark; the app assigns the per-session block index and stores it.
    CommandFinished(CommandCapture),

    /// An integrated-shell command began executing under a trusted OSC 133 lifecycle.
    /// Emitted at the command-output-start (`;C`) mark; the app tracks it as the
    /// session's in-flight block until `CommandFinished`, loss of boundary trust, or
    /// session exit clears it.
    CommandStarted(CommandStart),

    /// Reset to the default window title.
    ResetTitle,

    /// Request to store a text string in the clipboard.
    ClipboardStore(ClipboardType, String),

    /// Request to write the contents of the clipboard to the PTY.
    ///
    /// `route_id` identifies the panel that emitted the request so
    /// the bytes land on the originating PTY rather than whichever
    /// panel happens to be focused. The attached function is a
    /// formatter which transforms the clipboard content into the
    /// expected escape-sequence form.
    ClipboardLoad(
        usize,
        ClipboardType,
        Arc<dyn Fn(&str) -> String + Sync + Send + 'static>,
    ),

    /// Request to write the RGB value of a color to the PTY.
    ///
    /// `route_id` identifies the panel that emitted the request so
    /// the reply lands on the originating PTY. The attached function
    /// is a formatter which transforms the RGB color into the
    /// expected escape-sequence form.
    ColorRequest(
        usize,
        usize,
        Arc<dyn Fn(ColorRgb) -> String + Sync + Send + 'static>,
    ),

    /// Write some text to the PTY identified by `route_id`. Routing
    /// by panel (rather than the focused context) is required so
    /// CSI / OSC reply bytes land on the shell that asked for them
    /// even if the user focuses a different split mid-flight.
    PtyWrite(usize, String),

    /// Request to write the text area size to the PTY of `route_id`.
    TextAreaSizeRequest(
        usize,
        Arc<dyn Fn(WinsizeBuilder) -> String + Sync + Send + 'static>,
    ),

    /// Cursor blinking state has changed.
    CursorBlinkingChange,

    CursorBlinkingChangeOnRoute(usize),

    /// Progress bar report from OSC 9;4 sequence
    ProgressReport(ProgressReport),

    /// Terminal bell ring.
    Bell,

    /// Desktop notification from OSC 9 or OSC 777.
    DesktopNotification {
        title: String,
        body: String,
    },

    /// Shutdown request.
    Exit,

    /// Quit request.
    Quit,

    /// Leave current terminal.
    CloseTerminal(usize),

    BlinkCursor(u64, usize),

    /// Selection scroll tick — auto-scroll while dragging outside viewport.
    SelectionScrollTick,

    /// Update window titles.
    UpdateTitles,

    /// Update terminal screen colors.
    ///
    /// The first usize is the route_id, the second is the color index to change.
    /// Color index: 0 for foreground, 1 for background, 2 for cursor color.
    ColorChange(usize, usize, Option<ColorRgb>),

    // No operation
    Noop,
}

impl Debug for TerminalEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TerminalEvent::ClipboardStore(ty, text) => {
                write!(f, "ClipboardStore({ty:?}, {text})")
            }
            TerminalEvent::ClipboardLoad(route_id, ty, _) => {
                write!(f, "ClipboardLoad(route={route_id}, {ty:?})")
            }
            TerminalEvent::TextAreaSizeRequest(route_id, _) => {
                write!(f, "TextAreaSizeRequest(route={route_id})")
            }
            TerminalEvent::ColorRequest(route_id, index, _) => {
                write!(f, "ColorRequest(route={route_id}, idx={index})")
            }
            TerminalEvent::PtyWrite(route_id, text) => {
                write!(f, "PtyWrite(route={route_id}, {text})")
            }
            TerminalEvent::Title(title) => write!(f, "Title({title})"),
            TerminalEvent::TitleWithSubtitle(title, subtitle) => {
                write!(f, "TitleWithSubtitle({title}, {subtitle})")
            }
            TerminalEvent::InteractiveState(on) => write!(f, "InteractiveState({on})"),
            TerminalEvent::AltScreen(on) => write!(f, "AltScreen({on})"),
            TerminalEvent::PromptBoundaryTrusted(on) => write!(f, "PromptBoundaryTrusted({on})"),
            TerminalEvent::PromptStarted => write!(f, "PromptStarted"),
            TerminalEvent::CommandFinished(cmd) => {
                write!(
                    f,
                    "CommandFinished({}, exit={:?})",
                    cmd.command, cmd.exit_code
                )
            }
            TerminalEvent::CommandStarted(cmd) => {
                write!(f, "CommandStarted({})", cmd.command)
            }
            TerminalEvent::BlockBatch(events) => {
                write!(f, "BlockBatch({} events)", events.len())
            }
            TerminalEvent::Minimize(cond) => write!(f, "Minimize({cond})"),
            TerminalEvent::Hide => write!(f, "Hide)"),
            TerminalEvent::HideOtherApplications => write!(f, "HideOtherApplications)"),
            TerminalEvent::CursorBlinkingChange => write!(f, "CursorBlinkingChange"),
            TerminalEvent::CursorBlinkingChangeOnRoute(route_id) => {
                write!(f, "CursorBlinkingChangeOnRoute {route_id}")
            }
            TerminalEvent::ProgressReport(report) => {
                write!(f, "ProgressReport({:?})", report)
            }
            TerminalEvent::MouseCursorDirty => write!(f, "MouseCursorDirty"),
            TerminalEvent::ResetTitle => write!(f, "ResetTitle"),
            TerminalEvent::PrepareUpdateConfig => write!(f, "PrepareUpdateConfig"),
            TerminalEvent::PrepareRender(millis) => write!(f, "PrepareRender({millis})"),
            TerminalEvent::PrepareRenderOnRoute(millis, route) => {
                write!(f, "PrepareRender({millis} on route {route})")
            }
            TerminalEvent::Render => write!(f, "Render"),
            TerminalEvent::RenderRoute(route) => write!(f, "Render route {route}"),
            TerminalEvent::TerminalDamaged(route_id) => {
                write!(f, "TerminalDamaged route {route_id}")
            }
            TerminalEvent::GlyphProtocolQuery { route_id, cp } => {
                write!(f, "GlyphProtocolQuery route {route_id} cp {cp:#x}")
            }
            TerminalEvent::Bell => write!(f, "Bell"),
            TerminalEvent::DesktopNotification { title, body } => {
                write!(f, "DesktopNotification({title}, {body})")
            }
            TerminalEvent::Exit => write!(f, "Exit"),
            TerminalEvent::Quit => write!(f, "Quit"),
            TerminalEvent::CloseTerminal(route) => write!(f, "CloseTerminal {route}"),
            TerminalEvent::CreateWindow => write!(f, "CreateWindow"),
            TerminalEvent::CloseWindow => write!(f, "CloseWindow"),
            TerminalEvent::CreateNativeTab(_) => write!(f, "CreateNativeTab"),
            TerminalEvent::SelectNativeTabByIndex(tab_index) => {
                write!(f, "SelectNativeTabByIndex({tab_index})")
            }
            TerminalEvent::SelectNativeTabLast => write!(f, "SelectNativeTabLast"),
            TerminalEvent::SelectNativeTabNext => write!(f, "SelectNativeTabNext"),
            TerminalEvent::SelectNativeTabPrev => write!(f, "SelectNativeTabPrev"),
            TerminalEvent::CreateConfigEditor => write!(f, "CreateConfigEditor"),
            TerminalEvent::UpdateConfig => write!(f, "ReloadConfiguration"),
            TerminalEvent::ReportToAssistant(error_report) => {
                write!(f, "ReportToAssistant({})", error_report.report)
            }
            TerminalEvent::ToggleFullScreen => write!(f, "FullScreen"),
            TerminalEvent::ToggleAppearanceTheme => write!(f, "ToggleAppearanceTheme"),
            TerminalEvent::BlinkCursor(timeout, route_id) => {
                write!(f, "BlinkCursor {timeout} {route_id}")
            }
            TerminalEvent::SelectionScrollTick => write!(f, "SelectionScrollTick"),
            TerminalEvent::UpdateTitles => write!(f, "UpdateTitles"),
            TerminalEvent::Noop => write!(f, "Noop"),
            TerminalEvent::Copy(_) => write!(f, "Copy"),
            TerminalEvent::Paste => write!(f, "Paste"),
            TerminalEvent::UpdateFontSize(action) => write!(f, "UpdateFontSize({action:?})"),
            TerminalEvent::UpdateGraphics { route_id, .. } => {
                write!(f, "UpdateGraphics({route_id})")
            }
            TerminalEvent::ColorChange(route_id, color, rgb) => {
                write!(f, "ColorChange({route_id}, {color:?}, {rgb:?})")
            }
        }
    }
}

pub trait OnResize {
    fn on_resize(&mut self, window_size: WinsizeBuilder);
}

/// Event Loop for notifying the renderer about terminal events.
pub trait EventListener {
    fn event(&self) -> (Option<TerminalEvent>, bool);

    fn send_event(&self, _event: TerminalEvent, _id: WindowId) {}

    fn send_event_with_high_priority(&self, _event: TerminalEvent, _id: WindowId) {}

    fn send_redraw(&self, _id: WindowId) {}

    fn send_global_event(&self, _event: TerminalEvent) {}
}

#[derive(Clone)]
pub struct VoidListener;

impl From<TerminalEvent> for TerminalEventType {
    fn from(terminal_event: TerminalEvent) -> Self {
        Self::Terminal(terminal_event)
    }
}

impl EventListener for VoidListener {
    fn event(&self) -> (std::option::Option<TerminalEvent>, bool) {
        (None, false)
    }
}

/// Regex search state.
pub struct SearchState {
    /// Search direction.
    pub direction: Direction,

    /// Current position in the search history.
    pub history_index: Option<usize>,

    /// Search origin in viewport coordinates relative to original display offset.
    pub origin: Pos,

    /// Focused match during active search.
    pub focused_match: Option<Match>,

    /// Search regex and history.
    ///
    /// During an active search, the first element is the user's current input.
    ///
    /// While going through history, the [`SearchState::history_index`] will point to the element
    /// in history which is currently being previewed.
    pub history: VecDeque<String>,
}

impl SearchState {
    /// Search regex text if a search is active.
    pub fn regex(&self) -> Option<&String> {
        self.history_index.and_then(|index| self.history.get(index))
    }

    /// Direction of the search from the search origin.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Focused match during vi-less search.
    pub fn focused_match(&self) -> Option<&Match> {
        self.focused_match.as_ref()
    }

    /// Clear the focused match.
    pub fn clear_focused_match(&mut self) {
        self.focused_match = None;
    }

    /// Search regex text if a search is active.
    pub fn regex_mut(&mut self) -> Option<&mut String> {
        self.history_index
            .and_then(move |index| self.history.get_mut(index))
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            direction: Direction::Right,
            focused_match: Default::default(),
            history_index: Default::default(),
            history: Default::default(),
            origin: Default::default(),
        }
    }
}

/// Progress bar state for OSC 9;4 ConEmu/Windows Terminal progress reporting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    /// Remove/hide the progress bar (state 0)
    Remove,
    /// Set progress with a specific percentage (state 1)
    Set,
    /// Show error state (state 2)
    Error,
    /// Indeterminate/pulsing progress (state 3)
    Indeterminate,
    /// Paused progress (state 4)
    Pause,
}

/// Progress report from OSC 9;4 sequence
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressReport {
    /// The progress bar state
    pub state: ProgressState,
    /// Optional progress percentage (0-100), only used with Set, Error, and Pause states
    pub progress: Option<u8>,
}
