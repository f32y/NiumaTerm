use std::borrow::Cow;
use std::fmt::{self, Debug, Formatter};
use std::{path, time};

use futures::channel::oneshot;
use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_platform::WinsizeBuilder;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::error::SendError;

use crate::block_store::SegmentMeta;
use crate::clipboard::ClipboardType;
use crate::ghostty::{self, BlockHandle};
use crate::graphics::{GraphicData, UpdateQueues};
use crate::selection::SelectionType;
use crate::session::page::{PageSource, RowPage};

/// One PTY-thread block event: a trusted
/// `;D` freezes the whole command into a finished engine block
/// (`finish_block`, O(1) ownership transfer) and the app receives the
/// handle; the owner materializes requested rows into independent pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockEvent {
    /// The user cleared the terminal (announced in-band via the `;K` mark):
    /// the whole frozen history drops with the screen (the PTY side already
    /// cleared the engine blocks).
    HistoryCleared,
    /// A trusted `;D` froze the command into a finished engine block. The
    /// store keeps only the handle; rendering reads the block through
    /// `BlockRef`. `rows` is the row count at finish time, cached app-side
    /// so layout never needs an engine query. `meta` is the complete command
    /// record: the PTY thread holds command text, launch directory, timing,
    /// and exit code when it freezes the block, so the item arrives whole
    /// and the store never joins metadata by sequence number.
    EngineBlock {
        seq: u64,
        handle: ghostty::BlockHandle,
        rows: usize,
        meta: SegmentMeta,
    },
    /// The engine's current live block list, oldest first, with per-block
    /// row counts. Emitted after resize (eager reflow bumps generations and
    /// re-wraps rows) and after each finish (budget eviction may have
    /// dropped oldest blocks). The store prunes items whose handle is gone
    /// and refreshes cached rows/generation for the rest.
    EngineBlocksSync(Vec<(ghostty::BlockHandle, usize)>),
}

/// A completed integrated-shell command, captured from the OSC 133 lifecycle by the PTY
/// prompt sniffer. `command` is submitted shell text or a legacy echo estimate;
/// missing text does not prevent output from being retained.
/// `exit_code` comes from the `;D;<code>` argument (`None` for a foreign bare `;D`).
/// `cwd` is the **launch** working directory, latched at command start — the ps1 reports
/// the next prompt's OSC 7 just before `;D`, so a `cd` records its origin. Timestamps are
/// wall-clock at output-start (`;C`) and command-finished (`;D`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCapture {
    /// OSC 133 prompt-boundary sequence number of the block's prompt (`;A`),
    /// marrying this metadata to its block item (block-split).
    pub seq: u64,

    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub cwd: Option<path::PathBuf>,
    pub started_at: time::SystemTime,
    pub ended_at: time::SystemTime,
}

/// An integrated-shell execution beginning at a trusted `;C`, excluding an
/// explicitly empty submission. Carries the same launch metadata as the
/// eventual [`CommandCapture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandStart {
    /// See [`CommandCapture::seq`].
    pub seq: u64,

    pub command: Option<String>,
    pub cwd: Option<path::PathBuf>,
    pub started_at: time::SystemTime,
}

#[derive(Debug)]
pub enum Msg {
    /// Data that should be written to the PTY.
    Input(Cow<'static, [u8]>),
    Shutdown,
    Resize(WinsizeBuilder),
    Scroll(isize),
    ScrollTo(u64),
    ScrollToEnd,
    Theme(Box<Colors>),
    CursorShape {
        shape: CursorShape,
        reply: Reply<()>,
    },
    Query(Query),
    Checkpoint(CheckpointRequest),
    /// Update the local PowerShell resize workaround without waiting behind input.
    PowerShellCompatibility(bool),
}

/// Sends ordered commands and wakes the owning async terminal task.
#[derive(Clone)]
pub struct MsgSender {
    tx: UnboundedSender<Msg>,
}

impl MsgSender {
    pub fn new(tx: UnboundedSender<Msg>) -> Self {
        Self { tx }
    }

    pub fn send(&self, msg: Msg) -> Result<(), SendError<Msg>> {
        self.tx.send(msg)
    }
}

#[derive(Clone)]
pub enum TerminalEvent {
    /// New terminal content available.
    Render,
    /// Terminal content changed — lightweight notification (no damage payload).
    /// Damage versions travel in the published frame.
    TerminalDamaged(usize),
    /// Graphics update available from terminal.
    UpdateGraphics {
        route_id: usize,
        queues: UpdateQueues,
    },
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
    Cwd(String),
    ReadReady,
    /// Request to store a text string in the clipboard.
    ClipboardStore(ClipboardType, String),
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
    /// Leave current terminal.
    CloseTerminal(usize),
}

impl Debug for TerminalEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            TerminalEvent::ClipboardStore(ty, text) => {
                write!(f, "ClipboardStore({ty:?}, {text})")
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
                    "CommandFinished({:?}, exit={:?})",
                    cmd.command, cmd.exit_code
                )
            }
            TerminalEvent::CommandStarted(cmd) => {
                write!(f, "CommandStarted({:?})", cmd.command)
            }
            TerminalEvent::BlockBatch(events) => {
                write!(f, "BlockBatch({} events)", events.len())
            }
            TerminalEvent::ProgressReport(report) => {
                write!(f, "ProgressReport({:?})", report)
            }
            TerminalEvent::ResetTitle => write!(f, "ResetTitle"),
            TerminalEvent::ReadReady => f.write_str("ReadReady"),
            TerminalEvent::Cwd(cwd) => f.debug_tuple("Cwd").field(cwd).finish(),
            TerminalEvent::Render => write!(f, "Render"),
            TerminalEvent::TerminalDamaged(route_id) => {
                write!(f, "TerminalDamaged route {route_id}")
            }
            TerminalEvent::Bell => write!(f, "Bell"),
            TerminalEvent::DesktopNotification { title, body } => {
                write!(f, "DesktopNotification({title}, {body})")
            }
            TerminalEvent::Exit => write!(f, "Exit"),
            TerminalEvent::CloseTerminal(route) => write!(f, "CloseTerminal {route}"),
            TerminalEvent::UpdateGraphics { route_id, .. } => {
                write!(f, "UpdateGraphics({route_id})")
            }
        }
    }
}

/// Event Loop for notifying the renderer about terminal events.
///
/// Receives events synchronously during PTY processing. Implementations must
/// not wait for commands sent back to the same event loop, since parsing and
/// shutdown cannot proceed until the callback returns.
pub trait EventListener {
    fn send_event(&self, event: TerminalEvent);
}

#[derive(Clone)]
pub struct VoidListener;

impl EventListener for VoidListener {
    fn send_event(&self, _event: TerminalEvent) {}
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressReport {
    /// The progress bar state
    pub state: ProgressState,

    /// Optional progress percentage (0-100), only used with Set, Error, and Pause states
    pub progress: Option<u8>,
}

// ---------------------------------------------------------------------------
// Requests the session sends to the PTY thread
//
// The engine lives on the PTY thread, so every read of engine state from the
// UI side travels as a `Msg::Query` or `Msg::Checkpoint` carrying a oneshot
// reply. The PTY thread answers between output batches and marks a reply
// `Stale` when the frame or block it referred to has since moved on.
// ---------------------------------------------------------------------------

pub type Request<T> = oneshot::Receiver<Result<T, RequestError>>;

pub type Reply<T> = oneshot::Sender<Result<T, RequestError>>;

pub type BlockRange = ((usize, u32), (usize, u32));

#[derive(Debug, Clone)]
pub enum RequestError {
    Stale,
    Unavailable,
    Engine(String),
}

#[derive(Debug)]
pub struct TextPiece {
    pub handle: BlockHandle,
    pub start: Option<(usize, u32)>,
    pub end: Option<(usize, u32)>,
}

#[derive(Debug)]
pub enum TextSource {
    Screen {
        revision: u64,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    },
    Blocks(Vec<TextPiece>),
    BlockSelection {
        handle: BlockHandle,
        line: usize,
        col: u32,
        kind: SelectionType,
    },
}

#[derive(Debug)]
pub enum Query {
    Image {
        handle: BlockHandle,
        image_id: u32,
        reply: Reply<GraphicData>,
    },
    Rows {
        source: PageSource,
        start: usize,
        reply: Reply<RowPage>,
    },
    Text {
        source: TextSource,
        reply: Reply<String>,
    },
    ExpandSelection {
        handle: BlockHandle,
        line: usize,
        col: u32,
        kind: SelectionType,
        reply: Reply<BlockRange>,
    },
}

#[derive(Debug)]
pub struct Checkpoint {
    pub vt: Vec<u8>,
    pub cols: u16,
    pub rows: u16,
}

/// Completion runs on the owner thread before any later output is parsed.
/// It may register a stream subscriber but must not wait for another thread.
pub struct CheckpointRequest(pub Box<dyn FnOnce(Result<Checkpoint, RequestError>) + Send>);

impl fmt::Debug for CheckpointRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CheckpointRequest")
    }
}
