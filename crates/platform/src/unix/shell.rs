#[cfg(test)]
#[path = "shell_tests.rs"]
mod shell_tests;

use std::path::{Path, PathBuf};
use std::process::id;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{env, fs, io};

use tracing::warn;

use crate::PromptIntegration;
use crate::unix::hook_command::single_quoted;
use crate::unix::{ShellUser, environment, filesystem};

/// The shell launched when configuration names none.
///
/// `$SHELL` is what the user's session already chose; the password database is
/// the fallback for a process started without it (a launchd agent, a bare
/// `login` session). `/bin/sh` is the last resort every POSIX system has.
pub fn default_shell() -> String {
    ShellUser::from_env()
        .ok()
        .map(|user| user.shell)
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// How `shell` must be launched so it emits the bundled OSC 133 prompt marks,
/// or `None` when no integration is available for it.
///
/// Each shell is reached through the hook that leaves the set of the user's
/// startup files unchanged — turning the integration on must not add or drop
/// any of them. The files both shells need are materialized on first use.
pub fn prompt_integration(shell: Option<&str>) -> Option<PromptIntegration> {
    let shell = resolved_shell(shell);

    match shell_name(&shell)?.as_str() {
        "zsh" => zsh_integration(),
        "bash" => bash_integration(),
        _ => None,
    }
}

/// zsh's own startup files are suppressed with `NO_RCS` and the integration is
/// typed at the shell instead, so nothing about which files it finds depends on
/// `ZDOTDIR` — the user's stays untouched and the bootstrap replays their
/// startup sequence itself. `+Z` turns the line editor off so the line
/// discipline governs the echo of the bootstrap line, and
/// `HIST_IGNORE_SPACE` keeps that line, which carries a leading space, out of
/// the session's history; the bootstrap restores both.
fn zsh_integration() -> Option<PromptIntegration> {
    let directory = zsh_directory()?;

    Some(PromptIntegration {
        args: ["-f", "+Z", "-o", "histignorespace"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        environment: Vec::new(),
        bootstrap: Some(bootstrap_line(&directory.join(ZSH_HOOKS))),
    })
}

/// bash is handed its integration the same way, and for the same reason
/// nothing about which files it reads depends on `--rcfile` any more — which
/// matters, because bash ignores that flag as a login shell, and the macOS
/// launch is one.
///
/// The one difference from zsh is that the injected line cannot be hidden.
/// Suppressing the echo means starting without a line editor, and bash 3.2
/// cannot put one back: `set -o emacs` flips the option, but readline was
/// never initialized at startup and the shell stops printing prompts
/// altogether. So readline stays on and the bootstrap clears the screen
/// instead. `HISTCONTROL` still keeps the line out of the session's history;
/// it has no command-line form, so it travels in the environment with the
/// value it displaced, and the bootstrap puts that back.
fn bash_integration() -> Option<PromptIntegration> {
    let directory = bash_directory()?;

    let mut environment = vec![("HISTCONTROL".into(), "ignorespace".into())];

    if let Ok(saved) = env::var("HISTCONTROL") {
        environment.push(("NMT_SAVED_HISTCONTROL".into(), saved));
    }

    Some(PromptIntegration {
        args: vec!["--norc".into(), "--noprofile".into()],
        environment,
        bootstrap: Some(bootstrap_line(&directory.join(BASH_HOOKS))),
    })
}

/// The line typed at the shell to hand it the integration.
///
/// The leading space is what the launch's history setting keys on, and the
/// trailing newline is what submits it. Keeping the payload to one short line
/// is why the script lives in a file: a terminal's canonical input queue is
/// only guaranteed to hold a few hundred bytes.
fn bootstrap_line(script: &Path) -> String {
    format!(" source {}\n", single_quoted(&script.to_string_lossy()))
}

/// The configured shell, or the default when none is configured.
fn resolved_shell(shell: Option<&str>) -> String {
    match shell.map(str::trim).filter(|shell| !shell.is_empty()) {
        Some(shell) => shell.to_owned(),
        None => default_shell(),
    }
}

fn shell_name(shell: &str) -> Option<String> {
    Path::new(shell).file_name()?.to_str().map(str::to_owned)
}

const ZSH_HOOKS: &str = "nmt-integration.zsh";

const ZSH_FILES: [(&str, &str); 1] = [(
    ZSH_HOOKS,
    include_str!("../../../../assets/unix/zsh/nmt-integration.zsh"),
)];

const BASH_HOOKS: &str = "nmt-integration.bash";

const BASH_FILES: [(&str, &str); 1] = [(
    BASH_HOOKS,
    include_str!("../../../../assets/unix/bash/nmt-integration.bash"),
)];

fn zsh_directory() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

    DIR.get_or_init(|| installed("zsh", &ZSH_FILES)).as_deref()
}

fn bash_directory() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

    DIR.get_or_init(|| installed("bash", &BASH_FILES))
        .as_deref()
}

/// The materialized startup files for one shell, installed once per process.
///
/// A failure to write them yields `None` rather than a path: pointing a shell
/// at startup files that are not there would silently drop the user's own
/// configuration, which is far worse than running without the integration.
fn installed(shell: &str, files: &[(&str, &str)]) -> Option<PathBuf> {
    match install_files(shell, files) {
        Ok(dir) => Some(dir),
        Err(error) => {
            warn!("{shell} shell integration unavailable ({error})");

            None
        }
    }
}

fn install_files(shell: &str, files: &[(&str, &str)]) -> io::Result<PathBuf> {
    let dir = environment::data_dir()
        .join("shell-integration")
        .join(shell);

    fs::create_dir_all(&dir)?;

    for (name, contents) in files {
        write_atomically(&dir.join(name), contents)?;
    }

    Ok(dir)
}

/// Replace a startup file in one step.
///
/// Another instance may be installing the same directory while a shell is
/// starting up, and a shell that sources a half-written rc file loses the rest
/// of the user's configuration.
fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "startup file has no name"))?;

    // The pid separates concurrent installs by different instances, and the
    // counter separates concurrent calls inside one — two callers sharing a
    // staging path would rename each other's file away.
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let staging = path.with_file_name(format!(
        "{name}.{}-{}.staging",
        id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));

    fs::write(&staging, contents)?;

    filesystem::replace_file(&staging, path).inspect_err(|_| {
        let _ = fs::remove_file(&staging);
    })
}
