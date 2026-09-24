pub use crate::windows::readiness::SoftReady;
pub use crate::windows::shell_integration::{
    is_shell_integration_registered, register_shell_integration, set_system_notification_enabled,
    shell_integration_dll_mismatched, system_notification_enabled, unregister_shell_integration,
};

pub(crate) use crate::windows::notifier::{remove, show};
pub(crate) use crate::windows::powershell::{
    build_hook_command, default_shell, hook_command_contains, prompt_integration,
};

pub mod data_protection;
pub mod environment;
pub mod filesystem;
pub mod ipc;
pub mod powershell;
pub mod process;
pub mod window;

#[cfg(feature = "clipboard")]
pub(crate) mod clipboard;
pub(crate) mod library;

mod child;
mod conpty;
mod notifier;
mod pipes;
mod readiness;
mod registered_wait;
mod shell_integration;

#[cfg(test)]
mod tests;

use std::ffi::OsStr;
use std::future::Future;
use std::iter::{self, once};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll as TaskPoll, ready};
use std::{io, mem};

use tokio::task::{JoinHandle, spawn_blocking};

use crate::windows::child::ChildExitWatcher;
use crate::windows::conpty::Conpty as Backend;
use crate::windows::pipes::{ConinPipe as WritePipe, ConoutPipe as ReadPipe};
use crate::windows::process::{KillOnCloseJob, ProcessTree};
use crate::{AsyncPty, PtyOptions, WinsizeBuilder};

pub struct Pty {
    // Declared first so an unawaited drop starts the console close before the
    // output pipe closes; the host's final writes then fail instead of waiting.
    console: Console,
    conout: ReadPipe,
    conin: WritePipe,
    child_watcher: ChildExitWatcher,
}

/// Who holds the console: this value, a blocking resize, or the close started
/// by `poll_shutdown`. A resize and a close never run at the same time, and no
/// command can reach the console while another process owns it.
enum Console {
    Owned(Backend),
    Resizing(JoinHandle<Backend>),
    Closing(Pin<Box<dyn Future<Output = ()> + Send>>),
    Closed,
}

/// Create a ConPTY shell with child-only environment overrides.
pub fn create_pty_with_env(options: PtyOptions<'_>) -> Result<Pty, io::Error> {
    conpty::new(options, None)
}

/// Create a ConPTY whose child process tree is terminated when it is dropped.
pub fn create_managed_pty_with_env(options: PtyOptions<'_>) -> Result<Pty, io::Error> {
    conpty::new(options, Some(KillOnCloseJob::new()?))
}

impl Pty {
    fn new(
        backend: Backend,
        conout: ReadPipe,
        conin: WritePipe,
        child_watcher: ChildExitWatcher,
    ) -> Self {
        Self {
            console: Console::Owned(backend),
            conout,
            conin,
            child_watcher,
        }
    }

    pub fn process_tree(&self) -> Option<ProcessTree> {
        match &self.console {
            Console::Owned(backend) => backend.process_tree(),
            Console::Resizing(_) | Console::Closing(_) | Console::Closed => None,
        }
    }

    /// Reports whether a child exit has been observed without waiting.
    pub fn child_exited(&self) -> bool {
        self.child_watcher.exited()
    }

    /// Take the console back from a running resize once it finishes.
    fn poll_resize_completion(&mut self, cx: &mut Context<'_>) -> TaskPoll<io::Result<()>> {
        let Console::Resizing(task) = &mut self.console else {
            return TaskPoll::Ready(Ok(()));
        };

        match ready!(Pin::new(task).poll(cx)) {
            Ok(backend) => {
                self.console = Console::Owned(backend);

                TaskPoll::Ready(Ok(()))
            }
            Err(error) => {
                // The task dropped the console with its panic; the close it
                // started on drop is all the teardown left to run.
                self.console = Console::Closed;

                TaskPoll::Ready(Err(io::Error::other(error)))
            }
        }
    }
}

impl AsyncPty for Pty {
    fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> TaskPoll<io::Result<usize>> {
        self.conout.poll_read(cx, buf)
    }

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> TaskPoll<io::Result<usize>> {
        self.conin.poll_write(cx, buf)
    }

    fn poll_flush(&mut self, cx: &mut Context<'_>) -> TaskPoll<io::Result<()>> {
        self.conin.poll_flush(cx)
    }

    fn poll_exit(&mut self, cx: &mut Context<'_>) -> TaskPoll<()> {
        self.child_watcher.poll_exit(cx)
    }

    fn poll_resize(
        &mut self,
        cx: &mut Context<'_>,
        size: WinsizeBuilder,
    ) -> TaskPoll<io::Result<()>> {
        match mem::replace(&mut self.console, Console::Closed) {
            Console::Owned(mut backend) => {
                // The native control call can wait for the console host.
                // Transfer ownership while it runs so output reads keep
                // draining and no borrowed console handle can outlive its
                // owner on cancellation.
                self.console = Console::Resizing(spawn_blocking(move || {
                    backend.set_winsize((&size).into());

                    backend
                }));
            }
            // The caller repolls one request until it completes, so `size` is
            // the change already running.
            Console::Resizing(task) => self.console = Console::Resizing(task),
            closed => {
                self.console = closed;

                return TaskPoll::Ready(Err(io::Error::other("console is closed")));
            }
        }

        self.poll_resize_completion(cx)
    }

    fn poll_shutdown(&mut self, cx: &mut Context<'_>) -> TaskPoll<io::Result<()>> {
        // A running resize owns the console; the close needs it back first.
        ready!(self.poll_resize_completion(cx))?;

        self.console = match mem::replace(&mut self.console, Console::Closed) {
            Console::Owned(backend) => Console::Closing(Box::pin(backend.close())),
            other => other,
        };

        if let Console::Closing(closing) = &mut self.console {
            ready!(closing.as_mut().poll(cx));

            self.console = Console::Closed;
        }

        TaskPoll::Ready(Ok(()))
    }
}

fn command_line(shell: &str, args: &[String]) -> String {
    let shell = if shell.is_empty() {
        "powershell"
    } else {
        shell
    };

    // Without arguments the setting may be a whole legacy command line, which
    // must pass through as written. A value that names an existing file is a
    // bare program path, and left unquoted a space in it would make
    // CreateProcessW try each space-separated prefix as a program first.
    if args.is_empty() {
        return if Path::new(shell).is_file() {
            quote_command_arg(shell)
        } else {
            shell.to_string()
        };
    }

    let mut out = quote_command_arg(shell);

    for arg in args {
        out.push(' ');

        out.push_str(&quote_command_arg(arg));
    }

    out
}

fn quote_command_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c.is_whitespace() || c == '"') {
        return arg.to_string();
    }

    let mut out = String::with_capacity(arg.len() + 2);

    out.push('"');

    let mut backslashes = 0;

    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(iter::repeat_n('\\', backslashes * 2 + 1));

                out.push('"');

                backslashes = 0;
            }
            _ => {
                out.extend(iter::repeat_n('\\', backslashes));

                out.push(ch);

                backslashes = 0;
            }
        }
    }

    out.extend(iter::repeat_n('\\', backslashes * 2));

    out.push('"');

    out
}

/// Converts the string slice into a Windows-standard representation for "W"-
/// suffixed function variants, which accept UTF-16 encoded string values.
pub fn win32_string<S: AsRef<OsStr> + ?Sized>(value: &S) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}
