pub use conpty::{job_has_other_processes, job_other_process_count};
pub(crate) use notifier::{
    identity_registered, register_identity, remove, show, unregister_identity,
};
pub use readiness::SoftReady;
pub use shell_integration::{
    is_shell_integration_registered, register_shell_integration, set_system_notification_enabled,
    shell_integration_dll_mismatched, system_notification_enabled, unregister_shell_integration,
};

pub mod ipc;

mod child;
mod conpty;
mod notifier;
mod pipes;
mod readiness;
mod shell_integration;
mod spsc;

use std::ffi::OsStr;
use std::io::{self};
use std::iter::{self, once};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::TryRecvError;
use std::sync::{self};

use conpty::Conpty as Backend;
use pipes::{EventedAnonRead as ReadPipe, EventedAnonWrite as WritePipe};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

use crate::windows::child::ChildExitWatcher;
use crate::{
    ChildEvent, EventedPty, Interest, Poll, ProcessReadWrite, Token, Waker, Winsize, WinsizeBuilder,
};

pub struct Pty {
    // Backend is required to be the first field, to ensure correct drop order. Dropping
    // `conout` before `backend` will cause a deadlock (with Conpty).
    backend: Backend,
    conout: ReadPipe,
    conin: WritePipe,
    read_token: Token,
    write_token: Token,
    child_event_token: Token,
    child_watcher: ChildExitWatcher,
}

// Creates conpty instead of pty
// Windows Pseudo Console (ConPTY)
pub fn create_pty(
    shell: &str,
    args: Vec<String>,
    working_directory: &Option<String>,
    columns: u16,
    rows: u16,
) -> Result<Pty, io::Error> {
    create_pty_with_env(shell, args, working_directory, columns, rows, &[], None)
}

/// Create a ConPTY shell with explicit child-only environment overrides.
pub fn create_pty_with_env(
    shell: &str,
    args: Vec<String>,
    working_directory: &Option<String>,
    columns: u16,
    rows: u16,
    environment_overrides: &[(String, String)],
    starting_title: Option<&str>,
) -> Result<Pty, io::Error> {
    create_pty_with_management(
        shell,
        args,
        working_directory,
        columns,
        rows,
        environment_overrides,
        starting_title,
        crate::job_management(),
    )
}

/// Create a ConPTY whose entire child process tree is terminated when the PTY
/// is dropped. Background probes need deterministic cleanup regardless of the
/// user setting that controls process-tree management for ordinary terminals.
pub fn create_managed_pty_with_env(
    shell: &str,
    args: Vec<String>,
    working_directory: &Option<String>,
    columns: u16,
    rows: u16,
    environment_overrides: &[(String, String)],
    starting_title: Option<&str>,
) -> Result<Pty, io::Error> {
    let pty = create_pty_with_management(
        shell,
        args,
        working_directory,
        columns,
        rows,
        environment_overrides,
        starting_title,
        true,
    )?;
    if pty.job_handle().is_none() {
        return Err(io::Error::other(
            "managed ConPTY could not create its process-tree job",
        ));
    }
    Ok(pty)
}

fn create_pty_with_management(
    shell: &str,
    args: Vec<String>,
    working_directory: &Option<String>,
    columns: u16,
    rows: u16,
    environment_overrides: &[(String, String)],
    starting_title: Option<&str>,
    manage_process_tree: bool,
) -> Result<Pty, io::Error> {
    let exec = command_line(shell, &args);
    conpty::new(
        &exec,
        working_directory,
        columns,
        rows,
        environment_overrides,
        starting_title,
        manage_process_tree,
    )
}

impl Pty {
    fn new(
        backend: impl Into<Backend>,
        conout: impl Into<ReadPipe>,
        conin: impl Into<WritePipe>,
        child_watcher: ChildExitWatcher,
    ) -> Self {
        Self {
            backend: backend.into(),
            conout: conout.into(),
            conin: conin.into(),
            read_token: Token(0),
            write_token: Token(0),
            child_event_token: Token(0),
            child_watcher,
        }
    }

    /// The Job Object managing the shell's process tree, present when job
    /// management was enabled at spawn time (diagnostic/test accessor).
    pub fn job_handle(&self) -> Option<HANDLE> {
        self.backend.job()
    }

    pub fn child_watcher(&self) -> &ChildExitWatcher {
        &self.child_watcher
    }
}

impl ProcessReadWrite for Pty {
    type Reader = ReadPipe;
    type Writer = WritePipe;

    #[inline]
    fn register(
        &mut self,
        _poll: &Poll,
        token: &mut dyn Iterator<Item = Token>,
        _interest: Interest,
        waker: &sync::Arc<Waker>,
    ) -> io::Result<()> {
        self.read_token = token.next().unwrap();
        self.write_token = token.next().unwrap();
        self.child_event_token = token.next().unwrap();

        // ConPTY anon pipes have no real OS readiness source; the worker threads and
        // the child-exit callback signal the loop through this `Waker` instead.
        self.conout.soft().set_waker(waker.clone());
        self.conin.soft().set_waker(waker.clone());
        self.child_watcher.set_waker(waker.clone());

        Ok(())
    }

    #[inline]
    fn reregister(&mut self, _poll: &Poll, _interest: Interest) -> io::Result<()> {
        // Nothing to re-arm: the per-source soft-ready flags are level-like and the
        // worker threads keep them current. Write interest is implicit — the
        // conin flag is set whenever the buffer has space.
        Ok(())
    }

    #[inline]
    fn deregister(&mut self, _poll: &Poll) -> io::Result<()> {
        // No real OS sources were registered (the `Waker` is owned by the loop).
        Ok(())
    }

    #[inline]
    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.conout
    }

    #[inline]
    fn read_token(&self) -> Token {
        self.read_token
    }

    #[inline]
    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.conin
    }

    #[inline]
    fn write_token(&self) -> Token {
        self.write_token
    }

    #[inline]
    fn drain_ready(&self) -> Vec<Token> {
        let mut ready = Vec::with_capacity(3);
        if self.conout.soft().is_ready() {
            ready.push(self.read_token);
        }
        if self.conin.soft().is_ready() {
            ready.push(self.write_token);
        }
        if self.child_watcher.soft().is_ready() {
            ready.push(self.child_event_token);
        }
        ready
    }

    #[inline]
    fn has_ready(&self) -> bool {
        // Only sources with *unconsumed work* may force a zero-timeout spin: buffered
        // read data (conout) and a pending child-exit. Writability (conin) is excluded
        // on purpose — its flag is level-set to "buffer has space", which is true in
        // steady state, so including it would keep `has_ready()` permanently true and
        // make the event loop never block (100% CPU busy-spin). The write side is
        // re-armed by the worker's clear->set edge waker when the buffer drains, so it
        // does not need this spin path.
        self.conout.soft().is_ready() || self.child_watcher.soft().is_ready()
    }

    #[inline]
    fn set_winsize(&mut self, winsize_builder: WinsizeBuilder) -> Result<(), io::Error> {
        let winsize: Winsize = winsize_builder.build();
        self.backend.on_resize(winsize);
        Ok(())
    }
}

impl EventedPty for Pty {
    fn child_event_token(&self) -> Token {
        self.child_event_token
    }

    fn next_child_event(&mut self) -> Option<ChildEvent> {
        match self.child_watcher.event_rx().try_recv() {
            Ok(ev) => Some(ev),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(ChildEvent::Exited),
        }
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

fn cmdline(shell: &str) -> String {
    shell.to_string()
}

/// Converts the string slice into a Windows-standard representation for "W"-
/// suffixed function variants, which accept UTF-16 encoded string values.
pub fn win32_string<S: AsRef<OsStr> + ?Sized>(value: &S) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

pub fn spawn_daemon<I, S>(program: &str, args: I) -> io::Result<()>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr>,
{
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{command_line, quote_command_arg};

    #[test]
    fn command_line_quotes_shell_path_and_args() {
        let args = vec![
            "-NoExit".to_string(),
            "-Command".to_string(),
            r". 'C:\Program Files\NiumaTerm\assets\pwsh-integration.ps1'".to_string(),
        ];

        assert_eq!(
            command_line(r"C:\Program Files\PowerShell\7\pwsh.exe", &args),
            r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoExit -Command ". 'C:\Program Files\NiumaTerm\assets\pwsh-integration.ps1'""#
        );
    }

    #[test]
    fn command_line_keeps_legacy_raw_shell_when_args_are_empty() {
        assert_eq!(
            command_line("powershell -NoProfile -Command echo", &[]),
            "powershell -NoProfile -Command echo"
        );
    }

    #[test]
    fn quote_command_arg_handles_windows_argv_rules() {
        assert_eq!(quote_command_arg("pwsh.exe"), "pwsh.exe");
        assert_eq!(quote_command_arg(""), r#""""#);
        assert_eq!(
            quote_command_arg(r#"a "quoted" arg\"#),
            r#""a \"quoted\" arg\\""#
        );
    }
}
