pub use crate::windows::process_exit::wait_for_exit;
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
pub mod file_version;
pub mod filesystem;
pub mod ipc;
pub mod powershell;
pub mod process;
pub mod restart_manager;
pub mod self_update;
pub mod shell_extension;
pub mod window;

pub(crate) mod library;

mod child;
#[cfg(feature = "clipboard")]
mod clipboard;
mod conpty;
mod notifier;
mod pipes;
mod process_exit;
mod readiness;
mod shell_integration;

#[cfg(test)]
mod tests;

use std::ffi::OsStr;
use std::future::Future;
use std::io;
use std::iter::{self, once};
use std::os::windows::ffi::OsStrExt;
use std::pin::Pin;
use std::task::{Context, Poll as TaskPoll, ready};

use tokio::task::{JoinHandle, spawn_blocking};

use crate::windows::child::ChildExitWatcher;
use crate::windows::conpty::Conpty as Backend;
use crate::windows::pipes::{ConinPipe as WritePipe, ConoutPipe as ReadPipe};
use crate::windows::process::{KillOnCloseJob, ProcessTree};
use crate::{AsyncPty, PtyOptions, WinsizeBuilder};

pub struct Pty {
    // Declared first so an unawaited drop starts the console close before the
    // output pipe closes; the host's final writes then fail instead of waiting.
    backend: Option<Backend>,
    resize_task: Option<JoinHandle<Backend>>,

    /// The console close started by `poll_shutdown`, driven by its owner task.
    closing: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,

    conout: ReadPipe,
    conin: WritePipe,
    child_watcher: ChildExitWatcher,
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
        backend: impl Into<Backend>,
        conout: impl Into<ReadPipe>,
        conin: impl Into<WritePipe>,
        child_watcher: ChildExitWatcher,
    ) -> Self {
        Self {
            backend: Some(backend.into()),
            resize_task: None,
            closing: None,
            conout: conout.into(),
            conin: conin.into(),
            child_watcher,
        }
    }

    pub fn process_tree(&self) -> Option<ProcessTree> {
        self.backend.as_ref()?.process_tree()
    }

    /// Reports whether a child exit has been observed without waiting.
    pub fn child_exited(&self) -> bool {
        self.child_watcher.exited()
    }

    fn poll_resize_completion(&mut self, cx: &mut Context<'_>) -> TaskPoll<io::Result<()>> {
        let Some(task) = &mut self.resize_task else {
            return TaskPoll::Ready(Ok(()));
        };

        let result = match Pin::new(task).poll(cx) {
            TaskPoll::Pending => return TaskPoll::Pending,
            TaskPoll::Ready(result) => result,
        };

        self.resize_task = None;

        match result {
            Ok(backend) => {
                self.backend = Some(backend);

                TaskPoll::Ready(Ok(()))
            }
            Err(error) => TaskPoll::Ready(Err(io::Error::other(error))),
        }
    }
}

impl AsyncPty for Pty {
    fn start_async(&mut self) -> io::Result<()> {
        self.conout.start_async()
    }

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
        if self.resize_task.is_none() {
            let Some(mut backend) = self.backend.take() else {
                return TaskPoll::Ready(Err(io::Error::other("console backend is unavailable")));
            };

            // The native control call can wait for the console host. Transfer
            // ownership while it runs so output reads keep draining and no
            // borrowed console handle can outlive its owner on cancellation.
            self.resize_task = Some(spawn_blocking(move || {
                backend.set_winsize((&size).into());

                backend
            }));
        }

        self.poll_resize_completion(cx)
    }

    fn poll_shutdown(&mut self, cx: &mut Context<'_>) -> TaskPoll<io::Result<()>> {
        // A running resize owns the console; the close needs it back first.
        ready!(self.poll_resize_completion(cx))?;

        if let Some(backend) = self.backend.take() {
            self.closing = Some(Box::pin(backend.close()));
        }

        if let Some(closing) = &mut self.closing {
            ready!(closing.as_mut().poll(cx));

            self.closing = None;
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

    if args.is_empty() {
        return shell.to_string();
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
