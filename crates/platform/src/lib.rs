pub use mio::{Events, Interest, Poll, Token, Waker};

#[cfg(not(windows))]
pub use crate::unix::*;

#[cfg(windows)]
pub use crate::windows::*;

#[cfg(feature = "clipboard")]
pub mod clipboard;

pub mod library;

#[cfg(windows)]
pub mod windows;

#[cfg(not(windows))]
mod unix;

mod environment_override;

mod ipc_message;

mod process_lifetime;

use std::{io, sync};

/// The `mio` types this crate's `ProcessReadWrite`/`EventedPty` surface is built
/// on, re-exported so consumers drive the PTY event loop through
/// `nmt_platform::{Poll, ...}` without taking their own (possibly mismatched)
/// `mio` dependency.
use libc::c_ushort;

use mio::event::Event;

#[cfg(not(windows))]
use crate::unix as platform;

#[cfg(windows)]
use crate::windows as platform;

#[cfg(windows)]
use crate::windows::powershell::DEFAULT_CONFIG_SHELL;

/// Borrowed launch settings shared by local sessions and background PTYs.
#[derive(Clone, Copy)]
pub struct PtyOptions<'a> {
    pub shell: &'a str,
    pub args: &'a [String],
    pub working_directory: Option<&'a str>,
    pub columns: u16,
    pub rows: u16,
    pub environment_overrides: &'a [(String, String)],
    pub starting_title: Option<&'a str>,
    pub bootstrap: Option<&'a str>,
}

pub const APP_ID: &str = "NiumaTerm";
pub const USES_CONPTY: bool = cfg!(windows);

#[repr(C)]
pub struct Winsize {
    ws_row: c_ushort,
    ws_col: c_ushort,
    ws_xpixel: c_ushort,
    ws_ypixel: c_ushort,
}

pub trait ProcessReadWrite {
    type Reader: io::Read;

    /// Nonblocking input writer. `flush` reports `WouldBlock` while accepted
    /// bytes are still waiting for native writes, and completion must wake the
    /// registered poller. Successful flush does not mean the child consumed
    /// the input; it permits a subsequent resize to be submitted in order.
    type Writer: io::Write;

    fn reader(&mut self) -> &mut Self::Reader;

    fn read_token(&self) -> Token;

    fn writer(&mut self) -> &mut Self::Writer;

    fn write_token(&self) -> Token;

    fn set_winsize(&mut self, _: WinsizeBuilder) -> Result<(), io::Error>;

    /// Register the PTY's sources with the event loop's `Poll`, pulling tokens from
    /// the iterator. `waker` is the loop's `mio::Waker`: the Windows ConPTY worker
    /// threads have no real OS readiness source, so they signal "data ready" through
    /// this waker. The Unix path registers real fds and ignores the waker.
    fn register(
        &mut self,
        _: &Poll,
        _: &mut dyn Iterator<Item = Token>,
        _: Interest,
        _: &sync::Arc<Waker>,
    ) -> io::Result<()>;

    fn reregister(&mut self, _: &Poll, _: Interest) -> io::Result<()>;

    fn deregister(&mut self, _: &Poll) -> io::Result<()>;

    /// Tokens whose soft-ready flag is currently set (Windows ConPTY worker-thread
    /// readiness). The Unix path has real OS readiness and returns an empty iterator.
    /// The event loop feeds these through the same `match token` arms it uses for
    /// real `Poll` events.
    fn drain_ready(&self) -> Vec<Token>;

    /// Whether any soft-ready flag is currently set (Windows ConPTY level readiness),
    /// without allocating or clearing it. The event loop checks this before blocking
    /// in `poll()`: a `pty_read` capped by `MAX_LOCKED_READ` can return with data still
    /// in the ring (flag left set), and the worker only wakes on the clear→set edge, so
    /// a blocking `poll(None)` would sleep forever on already-signalled data. The Unix
    /// path has real OS readiness (re-armed by `EPOLL_CTL_MOD`) and returns `false`.
    fn has_ready(&self) -> bool {
        false
    }

    /// Whether readiness reports that the native PTY read side has closed.
    fn read_closed(&self, _event: &Event) -> bool {
        false
    }

    /// Whether a read error represents native PTY hangup.
    fn is_hangup_error(&self, _error: &io::Error) -> bool {
        false
    }
}

pub trait EventedPty: ProcessReadWrite {
    fn child_event_token(&self) -> Token;

    /// Reports whether a child exit has been observed without waiting.
    fn child_exited(&mut self) -> bool;
}

#[derive(Debug, Clone)]
pub struct WinsizeBuilder {
    pub rows: u16,
    pub cols: u16,
    pub width: u16,
    pub height: u16,
}

/// Unresolved configuration defaults preserve shell discovery at launch time.
pub fn configured_shell_defaults() -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        (DEFAULT_CONFIG_SHELL.into(), Vec::new())
    }

    #[cfg(not(windows))]
    {
        (String::new(), vec!["--login".into()])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeNotification {
    pub title: String,
    pub body: String,
    pub activation_url: String,
    pub tag: String,
    pub group: String,
}

pub fn show_notification(notification: &NativeNotification) -> Result<(), String> {
    platform::show(notification)
}

pub fn remove_notification(tag: &str, group: &str) -> Result<(), String> {
    platform::remove(tag, group)
}

/// The command line an agent writes into its hook config to invoke
/// `executable` with a single `argument`.
///
/// Agents run these entries through the platform's shell, so quoting and the
/// escape a path with spaces needs differ per platform; `argument` must stay
/// a bare identifier on every platform.
pub fn build_hook_command(executable: &str, argument: &str) -> io::Result<String> {
    platform::build_hook_command(executable, argument)
}

/// Whether `command` — a hook entry read back from an agent's config —
/// invokes the binary identified by `marker`.
///
/// Kept alongside the builder because a platform that encodes the command
/// (PowerShell's `-EncodedCommand`) has to decode it before the marker is
/// visible.
pub fn hook_command_contains(command: &str, marker: &str) -> bool {
    platform::hook_command_contains(command, marker)
}

/// The shell a terminal launches when configuration names none.
pub fn default_shell() -> String {
    platform::default_shell()
}

/// How a shell must be launched so it evaluates the bundled OSC 133 prompt
/// integration: startup arguments, child-only environment, or both. Which of
/// the two carries it is the platform's business — PowerShell takes the script
/// as an argument, zsh is reached through `ZDOTDIR`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptIntegration {
    pub args: Vec<String>,
    pub environment: Vec<(String, String)>,

    /// Bytes to place in the terminal's input queue before the shell starts,
    /// for a platform that hands the shell its integration by typing at it
    /// rather than through a startup file the shell would discover. The PTY
    /// hides them: the launch turns the shell's line editor off so the line
    /// discipline governs the echo, and the terminal clears `ECHO` for exactly
    /// this one write.
    pub bootstrap: Option<String>,
}

/// The launch adjustments that make `shell` report trusted prompt boundaries,
/// or `None` when the platform ships no integration for it. `None` for `shell`
/// means the platform's default shell.
///
/// A platform that has to materialize files does so on the first call, so a
/// `Some` answer means the launch is ready to go; a failure there reports
/// `None` rather than a launch that would drop the user's own configuration.
pub fn prompt_integration(shell: Option<&str>) -> Option<PromptIntegration> {
    platform::prompt_integration(shell)
}

impl From<&WinsizeBuilder> for Winsize {
    fn from(value: &WinsizeBuilder) -> Self {
        let ws_row = value.rows as c_ushort;
        let ws_col = value.cols as c_ushort;
        let ws_xpixel = value.width as c_ushort;
        let ws_ypixel = value.height as c_ushort;

        Winsize {
            ws_row,
            ws_col,
            ws_xpixel,
            ws_ypixel,
        }
    }
}

/// Resolve executable shims through the native shell on an interactive PTY.
pub fn interactive_shell_command(executable: &str, arguments: &[&str]) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        let mut args = vec!["/D".into(), "/C".into(), executable.into()];

        args.extend(arguments.iter().map(|argument| (*argument).to_string()));

        ("cmd.exe".into(), args)
    }

    #[cfg(not(windows))]
    {
        use crate::unix::hook_command::single_quoted;
        use std::iter;

        let words = iter::once(executable)
            .chain(arguments.iter().copied())
            .map(single_quoted)
            .collect::<Vec<_>>()
            .join(" ");

        (default_shell(), vec!["-c".into(), format!("exec {words}")])
    }
}
