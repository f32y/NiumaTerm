pub use crate::async_pty::{AsyncPty, poll_nonblocking};
#[cfg(not(windows))]
pub use crate::unix::*;
#[cfg(windows)]
pub use crate::windows::*;

#[cfg(feature = "clipboard")]
pub mod clipboard;
pub mod durable_file;
pub mod library;
#[cfg(target_os = "macos")]
pub mod macos_notifications;
#[cfg(windows)]
pub mod windows;

mod async_pty;
mod child_output;
mod environment_override;
mod ipc_message;
mod process_lifetime;
#[cfg(not(windows))]
mod unix;

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use libc::c_ushort;

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

/// A display name for [`default_shell`]: its file stem, with PowerShell's
/// executables spelled the way the product is. It names the built-in profile
/// and stands in for a tab title the shell has not set yet.
pub fn default_shell_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();

    NAME.get_or_init(|| {
        let shell = default_shell();

        let stem = Path::new(&shell)
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or(&shell);

        if stem.eq_ignore_ascii_case("powershell") || stem.eq_ignore_ascii_case("pwsh") {
            "PowerShell".to_string()
        } else {
            stem.to_string()
        }
    })
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
