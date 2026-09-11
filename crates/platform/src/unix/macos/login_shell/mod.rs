//! The part of the user's environment that a GUI launch never receives.
//!
//! An application started by launchd — from Finder, the Dock, or `open` — gets
//! a PATH of `/usr/bin:/bin:/usr/sbin:/sbin` and none of the variables the
//! user's shell startup files export. Terminal tabs do not notice: their PTY
//! child is started through `/usr/bin/login`, which builds a real login
//! environment for itself. A CLI this process spawns directly does notice,
//! because the executable lookup runs against this process's own PATH, so a
//! tool installed under the home directory (`~/.local/bin`, a Homebrew prefix,
//! a Node prefix) is not found at all and the spawn fails with `ENOENT`.
//!
//! Asking the user's login shell what it exports recovers those variables.

use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, process, thread};

use tracing::{debug, warn};

use crate::unix::shell::default_shell;

/// Separates shell startup chatter from the environment dump. Startup files
/// print banners, version notices and completion warnings, none of which can
/// be told apart from a variable assignment by shape alone.
///
/// A literal would not be enough on its own: a startup file that warns about
/// an unset variable prints the name it was asked about, and an exported
/// variable's value can hold arbitrary text. Neither can reproduce a string
/// the process invented for this one run.
fn marker() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());

    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);

    format!(
        "__NMT_LOGIN_ENVIRONMENT_{}_{clock}_{sequence}__",
        process::id()
    )
}

/// A startup file that waits on something — a network mount, a prompt the
/// shell will never receive an answer to — would otherwise block the first
/// agent launch forever. A heavyweight zsh configuration measures in the
/// hundreds of milliseconds, so this bound is generous without being open.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// Variables that describe the shell which produced the dump rather than the
/// user's environment. Carrying them over would describe the wrong process.
const SHELL_LOCAL: [&str; 4] = ["PWD", "OLDPWD", "SHLVL", "_"];

/// The variables a child of this process needs beyond the ones this process
/// inherited: everything the login shell exports that is missing here, plus
/// PATH, which launchd does supply but only as the system stub.
///
/// Empty when the application was started from a terminal, because that
/// environment is already the user's and replacing it would discard whatever
/// the surrounding shell session had set up.
///
/// The capture happens at most once per run: shell startup is slow enough to
/// be worth doing once, and an edit to a startup file takes effect on the next
/// launch either way.
pub(crate) fn missing_variables() -> &'static [(String, String)] {
    static VARIABLES: OnceLock<Vec<(String, String)>> = OnceLock::new();

    VARIABLES.get_or_init(|| {
        if started_from_terminal() {
            return Vec::new();
        }

        let shell = default_shell();

        let Some(captured) = capture(&shell) else {
            warn!("could not read the environment of login shell {shell}");

            return Vec::new();
        };

        let variables = importable(captured, |name| env::var_os(name).is_some());

        debug!(
            "imported {} variables from login shell {shell}",
            variables.len()
        );

        variables
    })
}

/// Whether this process was started from a terminal. A launchd start attaches
/// none of the three standard descriptors to a terminal device, while every
/// way of starting the application from a shell leaves at least one attached
/// even when the others are redirected.
fn started_from_terminal() -> bool {
    [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        // SAFETY: `isatty` only inspects the descriptor number it is given.
        .any(|descriptor| unsafe { libc::isatty(descriptor) } == 1)
}

/// Run `shell` far enough for it to evaluate the user's startup files, and
/// read back the environment they left behind.
///
/// The shell is both a login and an interactive one because there is no single
/// place users put their PATH: zsh spreads it over `.zshenv`, `.zprofile` and
/// `.zshrc`, and a bash user's is usually in `.bashrc`, which only an
/// interactive shell reads. `env -0` is what makes the dump parseable — a
/// value may contain newlines, so line-oriented output cannot be split back
/// apart. `exec` hands the process to `env` so that shell exit hooks cannot
/// append anything after the dump.
fn capture(shell: &str) -> Option<Vec<(String, String)>> {
    let marker = marker();
    let script = format!("printf '\\n{marker}\\n'; exec /usr/bin/env -0");

    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", script.as_str()])
        // An interactive shell reads from its input, so it is given one that
        // is immediately at end of file: a startup file that asks a question
        // gets an answer instead of waiting out the timeout, and the shell
        // cannot be stopped by SIGTTIN over a terminal it does not own.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Startup chatter and the complaints an interactive shell makes about
        // having no terminal are not part of the answer.
        .stderr(Stdio::null())
        // Own group, so the timeout can end a startup file's own children
        // rather than only the shell that is waiting on them.
        .process_group(0)
        .spawn()
        .inspect_err(|error| warn!("could not run login shell {shell}: {error}"))
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let mut output = Vec::new();
        let read = stdout.read_to_end(&mut output);
        let _ = sender.send(read.map(|_| output));
    });

    let captured = match receiver.recv_timeout(CAPTURE_TIMEOUT) {
        Ok(Ok(output)) => Some(output),

        Ok(Err(error)) => {
            warn!("could not read from login shell {shell}: {error}");

            None
        }

        Err(_) => {
            warn!("login shell {shell} did not finish within {CAPTURE_TIMEOUT:?}");

            // SAFETY: the group id is this child's pid, which stays reserved
            // until the `wait` below reaps it.
            unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };

            None
        }
    };

    let _ = child.wait();

    Some(parse(&captured?, &marker))
}

/// Split the NUL-separated dump that follows the marker into name/value pairs.
/// Anything ahead of the marker is startup output and is dropped.
fn parse(output: &[u8], marker: &str) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(output);

    let Some((_, dump)) = text.split_once(marker) else {
        return Vec::new();
    };

    dump.trim_start_matches('\n')
        .split('\0')
        .filter_map(|entry| entry.split_once('='))
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// Reduce a captured environment to what is worth carrying into a child.
///
/// `is_set` reports whether this process already has a variable. Those are
/// left alone: launchd's own values for `HOME`, `TMPDIR` or `SSH_AUTH_SOCK`
/// describe this session, and the shell only echoed them back. PATH is the
/// exception the whole capture exists for, because launchd always sets it and
/// always sets it to the system stub.
fn importable(
    variables: Vec<(String, String)>,
    is_set: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    variables
        .into_iter()
        .filter(|(name, _)| !SHELL_LOCAL.contains(&name.as_str()))
        .filter(|(name, _)| name == "PATH" || !is_set(name))
        .collect()
}

#[cfg(test)]
mod tests;
