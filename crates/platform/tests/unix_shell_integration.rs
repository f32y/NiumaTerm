#![cfg(unix)]
//! Drives a real shell through the production PTY path and reads the OSC 133
//! marks back out of the byte stream.
//!
//! The marks only earn boundary trust in a strict `A -> B -> C -> D` order, and
//! the pieces that produce them are spread across a `precmd` hook, a `preexec`
//! hook and a `PS1` suffix that a prompt framework may rebuild — and bash
//! reaches its own through a login hop that `exec`s a second shell. Nothing
//! short of running them shows whether the pieces still line up.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio, id};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use nmt_platform::{Pty, PtyOptions, create_pty_with_env, prompt_integration, terminfo_exists};

const DEADLINE: Duration = Duration::from_secs(20);

fn shell_path(name: &str) -> Option<String> {
    ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]
        .into_iter()
        .map(|dir| format!("{dir}/{name}"))
        .find(|candidate| Path::new(candidate).is_file())
}

/// The `133;` mark payloads in the order they appear: `A`, `B`, `C`, `D;0`.
fn marks(stream: &[u8]) -> Vec<String> {
    const INTRODUCER: &str = "\u{1b}]133;";

    let text = String::from_utf8_lossy(stream);

    let mut found = Vec::new();
    let mut rest = text.as_ref();

    while let Some(at) = rest.find(INTRODUCER) {
        rest = &rest[at + INTRODUCER.len()..];

        let end = rest.find(['\u{7}', '\u{1b}']).unwrap_or(rest.len());

        found.push(rest[..end].to_owned());

        rest = &rest[end..];
    }

    found
}

/// Command text is metadata; lifecycle ordering still depends on the C mark.
fn lifecycle_marks(seen: &[String]) -> Vec<&str> {
    seen.iter()
        .map(|mark| {
            if mark.starts_with("C;cmdline=") {
                "C"
            } else {
                mark.as_str()
            }
        })
        .collect()
}

struct Session {
    pty: Pty,
    home: PathBuf,

    /// Everything read so far, so a wait can be expressed against the whole
    /// session rather than against one read's worth of bytes.
    stream: Vec<u8>,
}

impl Session {
    /// Read until `done` accepts the marks seen so far, or the deadline passes.
    fn read_until(&mut self, done: impl Fn(&[String]) -> bool) -> Vec<String> {
        let mut buf = [0u8; 8192];

        let deadline = Instant::now() + DEADLINE;

        while Instant::now() < deadline {
            match self.pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    self.stream.extend_from_slice(&buf[..n]);

                    let seen = marks(&self.stream);

                    if done(&seen) {
                        return seen;
                    }
                }
                // The PTY is non-blocking, so "nothing yet" arrives as an
                // error rather than a short read.
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
        }

        marks(&self.stream)
    }

    /// Read until `needle` appears anywhere in the session, or the deadline
    /// passes. Reports whether it arrived.
    fn read_text_until(&mut self, needle: &str) -> bool {
        let mut buf = [0u8; 8192];

        let deadline = Instant::now() + DEADLINE;

        while Instant::now() < deadline {
            if String::from_utf8_lossy(&self.stream).contains(needle) {
                return true;
            }

            match self.pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.stream.extend_from_slice(&buf[..n]),
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
        }

        String::from_utf8_lossy(&self.stream).contains(needle)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.pty.write_all(b"exit\n");
        let _ = fs::remove_dir_all(&self.home);
    }
}

/// Start `shell` with the bundled integration and an empty HOME, so the marks
/// under test are the ones the bundled files emit rather than whatever the
/// developer's own configuration adds.
fn start(shell: &str, label: &str) -> Option<Session> {
    start_with_startup_files(shell, label, &[])
}

/// `startup_files` are written into the empty home before the shell runs, so a
/// test can check that the user's own configuration still reaches the session.
fn start_with_startup_files(
    shell: &str,
    label: &str,
    startup_files: &[(&str, &str)],
) -> Option<Session> {
    let Some(program) = shell_path(shell) else {
        eprintln!("skipping: no {shell} on this host");

        return None;
    };

    let integration = prompt_integration(Some(&program)).expect("the shell reports an integration");

    let home = env::temp_dir().join(format!("nmt-{shell}-{label}-{}", id()));

    fs::create_dir_all(&home).expect("temp home");

    for (name, contents) in startup_files {
        fs::write(home.join(name), contents).expect("startup file");
    }

    let home_value = home.to_string_lossy().into_owned();

    let mut environment = integration.environment;

    environment.push(("HOME".into(), home_value.clone()));

    // The shell inherits this process's `HISTFILE`, which would be the
    // developer's own; give each session its own so a history assertion sees
    // only what this session did.
    environment.push((
        "HISTFILE".into(),
        home.join("history").to_string_lossy().into_owned(),
    ));

    if shell == "zsh" {
        // `/usr/bin/login` resets HOME from the password database whatever the
        // caller passes, so an empty home only isolates zsh if the bootstrap
        // is pointed at it the way zsh itself would be. `login -p` does keep
        // ZDOTDIR.
        environment.push(("ZDOTDIR".into(), home_value));
    }

    match create_pty_with_env(PtyOptions {
        shell: &program,
        args: &integration.args,
        working_directory: None,
        columns: 80,
        rows: 24,
        environment_overrides: &environment,
        starting_title: None,
        bootstrap: integration.bootstrap.as_deref(),
    }) {
        Ok(pty) => Some(Session {
            pty,
            home,
            stream: Vec::new(),
        }),
        Err(error) => {
            eprintln!("skipping: could not spawn {shell}: {error:?}");

            let _ = fs::remove_dir_all(&home);

            None
        }
    }
}

fn assert_ordered_lifecycle(shell: &str) {
    let Some(mut session) = start(shell, "lifecycle") else {
        return;
    };

    // The synthetic prime (A, B, C), the D that closes it, then the real
    // prompt's own A and B.
    let primed = session.read_until(|seen| seen.len() >= 6);

    assert!(
        primed.len() >= 6,
        "the first prompt produced only {primed:?} within {DEADLINE:?}"
    );
    assert_eq!(
        &primed[..6],
        &["A", "B", "C", "D;0", "A", "B"],
        "the prime must complete an ordered lifecycle before the first prompt"
    );

    session.pty.write_all(b"true\n").expect("write command");

    // `preexec` closes the command line (C) and the next `precmd` reports the
    // exit status (D) before opening the following prompt (A).
    let after = session.read_until(|seen| seen.len() >= 9);

    assert!(
        after.len() >= 9,
        "running a command produced only {after:?} within {DEADLINE:?}"
    );
    assert_eq!(
        &lifecycle_marks(&after[6..9]),
        &["C", "D;0", "A"],
        "a command must run inside C -> D and be followed by the next prompt"
    );
    assert_eq!(after[6], "C;cmdline=dHJ1ZQ==");
}

#[test]
fn zsh_reports_an_ordered_prompt_lifecycle() {
    assert_ordered_lifecycle("zsh");
}

#[test]
fn bash_reports_an_ordered_prompt_lifecycle() {
    assert_ordered_lifecycle("bash");
}

/// The exit code is what a finished command block records, so a failure has to
/// travel out as its own status rather than a generic zero.
fn assert_reports_failing_exit_code(shell: &str) {
    let Some(mut session) = start(shell, "exit-code") else {
        return;
    };

    session.read_until(|seen| seen.len() >= 6);

    session.pty.write_all(b"false\n").expect("write command");

    let seen = session.read_until(|seen| seen.iter().any(|mark| mark == "D;1"));

    assert!(
        seen.iter().any(|mark| mark == "D;1"),
        "a failing command must report its status; saw {seen:?}"
    );
}

#[test]
fn zsh_reports_a_failing_commands_exit_code() {
    assert_reports_failing_exit_code("zsh");
}

#[test]
fn bash_reports_a_failing_commands_exit_code() {
    assert_reports_failing_exit_code("bash");
}

/// A user `clear` has to be announced in band: the terminal's own scrollback
/// is empty under the block protocol, so nothing else tells it the frozen
/// blocks should drop.
fn assert_announces_user_clear(shell: &str) {
    let Some(mut session) = start(shell, "clear") else {
        return;
    };

    session.read_until(|seen| seen.len() >= 6);

    session.pty.write_all(b"clear\n").expect("write command");

    let seen = session.read_until(|seen| seen.iter().any(|mark| mark == "K"));

    assert!(
        seen.iter().any(|mark| mark == "K"),
        "clear must announce itself before erasing; saw {seen:?}"
    );
}

#[test]
fn zsh_announces_a_user_clear() {
    assert_announces_user_clear("zsh");
}

#[test]
fn bash_announces_a_user_clear() {
    assert_announces_user_clear("bash");
}

/// The integration must not cost the user their own configuration: the launch
/// suppresses zsh's startup files, so the bootstrap has to put every one of
/// them back.
#[test]
fn zsh_still_reads_the_users_startup_files() {
    let Some(mut session) = start_with_startup_files(
        "zsh",
        "startup",
        &[(".zshrc", "export NMT_TEST_STARTUP=reached\n")],
    ) else {
        return;
    };

    session.read_until(|seen| seen.len() >= 6);

    session
        .pty
        .write_all(b"printf 'marker=[%s]\\n' \"$NMT_TEST_STARTUP\"\n")
        .expect("write command");

    let deadline = Instant::now() + DEADLINE;

    let mut buf = [0u8; 8192];

    while Instant::now() < deadline {
        match session.pty.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                session.stream.extend_from_slice(&buf[..n]);

                if String::from_utf8_lossy(&session.stream).contains("marker=[reached]") {
                    return;
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }

    panic!(
        "zsh did not read the user's startup files; saw {}",
        String::from_utf8_lossy(&session.stream)
    );
}

/// The same for bash. Run without a terminal the shell is not a login shell,
/// which is the branch a temp home can control: `/usr/bin/login`, which the
/// macOS PTY path goes through, resets `$HOME` from the password database no
/// matter what the caller passes.
fn bash_bootstrap_in_temp_home(label: &str, files: &[(&str, &str)], probe: &str) -> String {
    let Some(bash) = shell_path("bash") else {
        eprintln!("skipping: no bash on this host");

        return "<skipped>".into();
    };

    let integration = prompt_integration(Some(&bash)).expect("bash is integrated");
    let bootstrap = integration.bootstrap.expect("bash is bootstrapped");

    // The injected line is `<space>source '<path>'<newline>`; the script it
    // names is what a shell without a terminal can be handed directly.
    let script = bootstrap
        .trim()
        .trim_start_matches("source ")
        .trim_matches('\'')
        .to_owned();

    let home = env::temp_dir().join(format!("nmt-bash-{label}-{}", id()));

    fs::create_dir_all(&home).expect("temp home");

    for (name, contents) in files {
        fs::write(home.join(name), contents).expect("startup file");
    }

    let output = Command::new(&bash)
        .args(["--norc", "--noprofile", "-c"])
        .arg(format!("source '{script}'; {probe}"))
        .env("HOME", home.to_string_lossy().into_owned())
        .env_remove("NMT_TEST_STARTUP")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("run bash");

    let _ = fs::remove_dir_all(&home);

    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn bash_still_reads_the_users_startup_files() {
    let text = bash_bootstrap_in_temp_home(
        "startup",
        &[(".bashrc", "export NMT_TEST_STARTUP=reached\n")],
        "printf 'marker=[%s]\\n' \"$NMT_TEST_STARTUP\"",
    );

    assert!(
        text.contains("marker=[reached]") || text == "<skipped>",
        "bash did not read the user's startup files; saw {text}"
    );
}

/// The user's shell has to end up with everything their files defined, not
/// just what an `export` would have carried.
#[test]
fn bash_keeps_functions_and_aliases_from_the_users_startup_files() {
    let text = bash_bootstrap_in_temp_home(
        "funcs",
        &[(
            ".bashrc",
            "nmt_probe_func() { :; }\nalias nmt_probe_alias='true'\n",
        )],
        "printf 'func=[%s] alias=[%s]\\n' \"$(type -t nmt_probe_func)\" \
         \"$(alias nmt_probe_alias >/dev/null 2>&1 && echo yes)\"",
    );

    assert!(
        text.contains("func=[function] alias=[yes]") || text == "<skipped>",
        "the user's functions and aliases did not survive the launch; saw {text}"
    );
}

/// bash allows one DEBUG trap, and the integration installs one for `;C`. A
/// trap the user's own files put there — bash-preexec, atuin — must keep
/// firing rather than be silently replaced.
#[test]
fn bash_keeps_a_debug_trap_the_user_already_installed() {
    let text = bash_bootstrap_in_temp_home(
        "debugtrap",
        &[(
            ".bashrc",
            "trap 'printf \"USERTRAP[%s]\\n\" \"$BASH_COMMAND\"' DEBUG\n",
        )],
        "echo probe-command",
    );

    assert!(
        text.contains("USERTRAP[echo probe-command]") || text == "<skipped>",
        "the user's DEBUG trap stopped firing; saw {text}"
    );
}

/// Pressing Enter on an empty line must not cost boundary trust: no command
/// runs, so the shell's pre-execution hook never fires, and a `;D` arriving
/// straight after `;B` is an out-of-order lifecycle.
fn assert_empty_enter_keeps_the_lifecycle_ordered(shell: &str) {
    let Some(mut session) = start(shell, "empty-enter") else {
        return;
    };

    session.read_until(|seen| seen.len() >= 6);

    session.pty.write_all(b"\n").expect("write empty line");

    let seen = session.read_until(|seen| seen.len() >= 9);

    assert_eq!(
        &lifecycle_marks(&seen[6..9]),
        &["C", "D;0", "A"],
        "an empty line must still close its command region; saw {seen:?}"
    );
    assert_eq!(seen[6], "C;cmdline=");
}

#[test]
fn zsh_empty_enter_keeps_the_lifecycle_ordered() {
    assert_empty_enter_keeps_the_lifecycle_ordered("zsh");
}

#[test]
fn bash_empty_enter_keeps_the_lifecycle_ordered() {
    assert_empty_enter_keeps_the_lifecycle_ordered("bash");
}

/// The prompt-end mark is re-applied on every prompt, so the strip that
/// precedes it has to actually match: without it PS1 would grow by one marker
/// per prompt, and the terminal would see the prompt region close early.
fn assert_the_prompt_mark_does_not_accumulate(shell: &str) {
    let Some(mut session) = start(shell, "no-accumulate") else {
        return;
    };

    session.read_until(|seen| seen.len() >= 6);

    for _ in 0..4 {
        session.pty.write_all(b"true\n").expect("write command");
    }

    // Four commands past the priming six marks: C, D, A, B each.
    let seen = session.read_until(|seen| seen.len() >= 6 + 4 * 4);

    assert_eq!(
        &lifecycle_marks(&seen[6..]),
        &[
            "C", "D;0", "A", "B", "C", "D;0", "A", "B", "C", "D;0", "A", "B", "C", "D;0", "A", "B"
        ],
        "each prompt must carry exactly one B; saw {seen:?}"
    );
}

#[test]
fn zsh_prompt_mark_does_not_accumulate() {
    assert_the_prompt_mark_does_not_accumulate("zsh");
}

#[test]
fn bash_prompt_mark_does_not_accumulate() {
    assert_the_prompt_mark_does_not_accumulate("bash");
}

/// The line the terminal types at the shell must not end up in the user's
/// history. It carries a leading space and the launch sets the shell's
/// ignore-space setting for exactly that; the bootstrap then puts the setting
/// back, so nothing but that one line is affected.
///
/// Checked against the history file the session writes rather than the byte
/// stream: on bash the injected line is echoed by readline — cleared from the
/// screen, not from the stream — so the stream is the wrong thing to look at.
///
/// Only bash is covered end to end. zsh writes no history file unless
/// `SAVEHIST` is set from a startup file, and arranging that inside a session
/// whose startup files the launch deliberately suppresses ends up pinning the
/// arrangement rather than the behaviour. The zsh side is covered by the unit
/// test that pins `-o histignorespace` into the launch.
fn assert_the_bootstrap_line_leaves_no_history(shell: &str, write_history: &[u8]) {
    let Some(mut session) = start(shell, "history") else {
        return;
    };

    let history_file = session.home.join("history");

    session.read_until(|seen| seen.len() >= 6);

    // The entry lingers until a later command is entered, so one has to be.
    session.pty.write_all(b"true\n").expect("write command");

    session.read_until(|seen| seen.len() >= 10);

    session
        .pty
        .write_all(write_history)
        .expect("write history flush");

    session.read_until(|seen| seen.len() >= 14);

    let written = fs::read_to_string(&history_file)
        .unwrap_or_else(|error| panic!("the session wrote no history file: {error}"));

    assert!(
        written.contains("true"),
        "the history file holds nothing this session ran: {written}"
    );
    assert!(
        !written.contains("nmt-integration"),
        "the bootstrap line reached the session history: {written}"
    );
}

#[test]
fn bash_bootstrap_line_leaves_no_history() {
    assert_the_bootstrap_line_leaves_no_history("bash", b"history -w\n");
}

/// zsh draws its right prompt after the left one, so its bytes arrive inside
/// the command-echo region. The integration closes it with a second `;B`,
/// which has to leave the lifecycle ordered — a repeated mark that the
/// terminal rejected would cost boundary trust on every prompt.
#[test]
fn zsh_right_prompt_closes_with_its_own_command_mark() {
    let Some(mut session) = start_with_startup_files(
        "zsh",
        "rprompt",
        &[(".zshrc", "PS1='LP> '\nRPROMPT='RIGHTPROMPT'\n")],
    ) else {
        return;
    };

    // The prime, the first prompt's `;D`/`;A`, then both `;B`s: the left
    // prompt's and the right prompt's.
    let primed = session.read_until(|seen| seen.len() >= 7);

    assert_eq!(
        &primed[..7],
        &["A", "B", "C", "D;0", "A", "B", "B"],
        "the right prompt must close with its own command mark; saw {primed:?}"
    );

    session.pty.write_all(b"true\n").expect("write command");

    let after = session.read_until(|seen| seen.len() >= 10);

    assert_eq!(
        &lifecycle_marks(&after[7..10]),
        &["C", "D;0", "A"],
        "the repeated mark must leave the lifecycle ordered; saw {after:?}"
    );
}

/// A session states which terminal it is rather than passing on whatever
/// started the application. Started from Finder or the Dock there is nothing to
/// pass on, and `/usr/bin/login` fills the gap with `network`, a name no
/// terminfo database carries; the shell then decides it cannot address the
/// cursor and reprints its prompt instead of redrawing it in place.
///
/// The angle brackets separate the answer from the echo of the command that
/// asks for it, which carries the format string rather than the value.
#[test]
fn a_session_tells_its_shell_which_terminal_it_is() {
    let Some(mut session) = start("zsh", "term") else {
        return;
    };

    session.read_until(|seen| seen.len() >= 6);

    session
        .pty
        .write_all(b"printf '<%s>\\n' \"$TERM\"\n")
        .expect("write command");

    let expected = if terminfo_exists("xterm-256color") {
        "<xterm-256color>"
    } else {
        "<xterm>"
    };

    assert!(
        session.read_text_until(expected),
        "the shell reported no {expected}; the session showed {}",
        String::from_utf8_lossy(&session.stream)
    );
}
