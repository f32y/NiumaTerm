use std::borrow::Cow;
use std::fmt::{self, Debug, Formatter};
use std::sync::{self, Arc};
use std::{path, time};

use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_platform::{Waker, WinsizeBuilder};

use crate::clipboard::ClipboardType;
use crate::ghostty;
use crate::graphics::UpdateQueues;
use crate::session::request::{CheckpointRequest, Query, Reply};

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
    /// so layout never needs an engine query.
    EngineBlock {
        seq: u64,
        handle: ghostty::BlockHandle,
        rows: usize,
    },
    /// The engine's current live block list, oldest first, with per-block
    /// row counts. Emitted after resize (eager reflow bumps generations and
    /// re-wraps rows) and after each finish (budget eviction may have
    /// dropped oldest blocks). The store prunes items whose handle is gone
    /// and refreshes cached rows/generation for the rest.
    EngineBlocksSync(Vec<(ghostty::BlockHandle, usize)>),
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
    pub cwd: Option<path::PathBuf>,
    pub started_at: time::SystemTime,
    pub ended_at: time::SystemTime,
}

/// An integrated-shell command that just began executing (trusted `;C` with a non-empty
/// echo), for the in-flight command block. Carries the same launch metadata as the
/// eventual [`CommandCapture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandStart {
    /// See [`CommandCapture::seq`].
    pub seq: u64,

    pub command: String,
    pub cwd: Option<path::PathBuf>,
    pub started_at: time::SystemTime,
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

/// A `Msg` sender that wakes the PTY event loop's mio `Poll` after each send, so the
/// loop re-polls and drains the receiver. mio 1.2 has no pollable channel, so the
/// `std::sync::mpsc` channel is paired with the loop's `Waker`.
#[derive(Clone)]
pub struct MsgSender {
    tx: sync::mpsc::Sender<Msg>,
    waker: Arc<Waker>,
}

impl MsgSender {
    pub fn new(tx: sync::mpsc::Sender<Msg>, waker: Arc<Waker>) -> Self {
        Self { tx, waker }
    }

    pub fn send(&self, msg: Msg) -> Result<(), sync::mpsc::SendError<Msg>> {
        self.tx.send(msg)?;

        // Wake the loop so it drains the receiver. A failed wake means the loop is gone.
        let _ = self.waker.wake();

        Ok(())
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

impl From<TerminalEvent> for TerminalEventType {
    fn from(terminal_event: TerminalEvent) -> Self {
        Self::Terminal(terminal_event)
    }
}

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
