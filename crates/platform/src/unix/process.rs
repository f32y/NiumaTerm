use std::ffi::{OsStr, OsString};
#[cfg(not(target_os = "macos"))]
use std::fs;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Weak};
use std::{env, io};

#[cfg(target_os = "macos")]
use crate::unix::macos::login_shell;
#[cfg(target_os = "macos")]
use crate::unix::macos::process_group_count;

/// A command that leads its own process group.
///
/// There is no console window to suppress on Unix, but the containment half of
/// the Windows counterpart still applies: a child in its own group can be
/// signalled as a unit without the signal reaching this process, which is what
/// [`KillOnCloseJob`] relies on.
pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.process_group(0);
    command
}

/// Run `executable` through the shell lookup rules of the platform.
///
/// Windows needs a `cmd.exe` hop so `PATHEXT` resolves the `.cmd` shims that
/// Node-based tools install; `execvp` already searches `PATH` for a bare name,
/// so the extra hop would only add a process that swallows signals.
///
/// The child carries the environment a GUI launch did not inherit, PATH
/// included. A bare name is resolved against the child's own PATH rather than
/// against this process's, so setting it here is what lets a tool installed
/// under the user's home directory be found at all.
#[cfg(target_os = "macos")]
pub fn hidden_cmd_command(executable: impl AsRef<OsStr>) -> Command {
    let mut command = hidden_command(executable);
    command.envs(
        login_shell::missing_variables()
            .iter()
            .map(|(name, value)| (name, value)),
    );
    command
}

/// Run `executable` through the shell lookup rules of the platform.
///
/// `execvp` already searches `PATH` for a bare name, so nothing needs to be
/// added on top of it.
#[cfg(not(target_os = "macos"))]
pub fn hidden_cmd_command(executable: impl AsRef<OsStr>) -> Command {
    hidden_command(executable)
}

/// The value `name` carries in a child started by [`hidden_cmd_command`].
///
/// A caller that resolves an executable itself rather than leaving it to the
/// spawn has to search the same PATH the spawn would, or it reports a tool as
/// missing that the spawn would have found.
#[cfg(target_os = "macos")]
pub fn launch_env_var(name: &str) -> Option<OsString> {
    login_shell::missing_variables()
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| OsString::from(value))
        .or_else(|| env::var_os(name))
}

/// The value `name` carries in a child started by [`hidden_cmd_command`],
/// which inherits this process's environment unchanged.
#[cfg(not(target_os = "macos"))]
pub fn launch_env_var(name: &str) -> Option<OsString> {
    env::var_os(name)
}

/// Text of one child's stdout or stderr capture.
///
/// Unix children write in the locale encoding, which is UTF-8 on every
/// supported system; invalid sequences are replaced so a capture cut
/// mid-character still yields its readable prefix.
pub fn decode_child_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Build the status a process that exited with `code` would report.
///
/// `ExitStatus` wraps a `wait` status rather than the exit code, and the exit
/// code lives in the upper byte of the low 16 bits.
pub fn exit_status_from_code(code: u32) -> ExitStatus {
    ExitStatus::from_raw(((code & 0xff) as i32) << 8)
}

/// Owns the process group a child leads and kills the whole group when the
/// last handle drops, so a command shim and its descendants share one
/// lifetime.
pub struct KillOnCloseJob(Arc<ProcessGroup>);

struct ProcessGroup(libc::pid_t);

/// A non-owning view of a [`KillOnCloseJob`]'s group. It never keeps the group
/// alive, so the count drops to zero once the owner releases it.
#[derive(Clone)]
pub struct ProcessTree(Weak<ProcessGroup>);

impl KillOnCloseJob {
    pub fn attach(child: &Child) -> io::Result<Self> {
        let pid = child.id() as libc::pid_t;

        // Idempotent when the command already asked for its own group; the
        // call is what covers a `Command` built without `process_group`. The
        // kernel refuses it once the child has exec'd (`EACCES`) or made
        // itself a session leader (`EPERM`) — both mean the child settled its
        // own group, so membership is read back rather than assumed.
        // SAFETY: both arguments are plain integers.
        if unsafe { libc::setpgid(pid, pid) } != 0 {
            let error = io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::EACCES) | Some(libc::EPERM)) {
                return Err(error);
            }
        }

        // SAFETY: `pid` is a live child; this process has not reaped it yet.
        let group = unsafe { libc::getpgid(pid) };
        if group != pid {
            return Err(io::Error::other(
                "child process does not lead its own process group",
            ));
        }

        Ok(Self(Arc::new(ProcessGroup(pid))))
    }

    pub fn attach_or_kill(child: &mut Child) -> io::Result<Self> {
        Self::attach(child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })
    }

    pub(crate) fn process_tree(&self) -> ProcessTree {
        ProcessTree(Arc::downgrade(&self.0))
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: the group id is one this process created and never reused.
        unsafe { libc::killpg(self.0, libc::SIGKILL) };
    }
}

impl ProcessTree {
    pub fn process_count(&self) -> usize {
        self.0
            .upgrade()
            .map_or(0, |group| group_process_count(group.0))
    }

    pub fn other_process_count(&self) -> usize {
        self.process_count().saturating_sub(1)
    }
}

#[cfg(target_os = "macos")]
fn group_process_count(pgid: libc::pid_t) -> usize {
    process_group_count(pgid)
}

/// Linux exposes the group of a process only through `/proc/<pid>/stat`, so
/// membership is counted by scanning the live pids. Field 5 is the group id,
/// and it follows the comm field, which may itself contain spaces or
/// parentheses — hence the split on the last `')'` rather than on whitespace.
#[cfg(not(target_os = "macos"))]
fn group_process_count(pgid: libc::pid_t) -> usize {
    let Ok(entries) = fs::read_dir("/proc") else {
        return 0;
    };

    entries
        .flatten()
        .filter(|entry| {
            let Ok(status) = fs::read_to_string(entry.path().join("stat")) else {
                return false;
            };
            let Some((_, after_comm)) = status.rsplit_once(')') else {
                return false;
            };
            after_comm
                .split_whitespace()
                .nth(2)
                .and_then(|field| field.parse::<libc::pid_t>().ok())
                == Some(pgid)
        })
        .count()
}
