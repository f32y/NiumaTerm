//! Regression tests for the two vtebench-reported block-mode bugs (real
//! ConPTY + real PowerShell + the bundled OSC 133 integration script):
//!
//! 1. A command interrupted (or exiting) while the engine is on the alternate
//!    screen left `AltScreen(true)` latched forever — block mode never
//!    re-engaged at the next prompt.
//! 2. A command that ends its output with a RIS (`ESC c`, what vtebench writes
//!    between samples and before printing results) lost the post-RIS output:
//!    the results text showed up neither in the frozen block nor on screen.
//!
//! Both bugs are fixed; these tests pin the recovered behavior. Environments
//! where the real shell session cannot start (no ConPTY, missing PowerShell)
//! skip via the `trusted_session()` guard rather than failing.

#![cfg(windows)]

use std::time::{Duration, Instant};
use std::{path, thread};

use nmt_terminal::ghostty::BlockHandle;

use super::session::{HostEvent, TerminalSession, TerminalSessionConfig};

fn integration_config() -> TerminalSessionConfig {
    // The test binary has no exe-relative assets dir; point at the repo script.
    // No canonicalize: its `\\?\` prefix breaks PowerShell dot-sourcing.
    let script = path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..\\..\\assets\\windows\\pwsh-integration.ps1");

    assert!(script.exists(), "missing {}", script.display());

    TerminalSessionConfig {
        shell: Some("powershell.exe".into()),
        args: vec![
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-NoExit".into(),
            "-Command".into(),
            // Synthetic commands run through PSReadLine, so keep them out of the
            // user's shared ConsoleHost history while this test session is active.
            format!(
                "Set-PSReadLineOption -HistorySaveStyle SaveNothing; . '{}'",
                script.display()
            ),
        ],
        cols: 100,
        rows: 30,
        ..TerminalSessionConfig::default()
    }
}

/// Drain new host events into `all`, returning how many arrived.
fn pump(session: &TerminalSession, all: &mut Vec<HostEvent>) -> usize {
    let new = session.poll_events();
    let n = new.len();

    all.extend(new);

    n
}

/// Poll until `pred(all_events)` or timeout; returns whether it held.
fn wait_for(
    session: &TerminalSession,
    all: &mut Vec<HostEvent>,
    timeout: Duration,
    mut pred: impl FnMut(&[HostEvent]) -> bool,
) -> bool {
    let end = Instant::now() + timeout;

    loop {
        pump(session, all);

        if pred(all) {
            return true;
        }

        if Instant::now() >= end {
            return false;
        }

        thread::sleep(Duration::from_millis(50));
    }
}

fn screen_text(session: &TerminalSession) -> String {
    let b = session.render_buffer.lock();
    (0..b.rows())
        .map(|y| {
            (0..b.cols())
                .map(|x| b.cell(x, y).c())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn block_texts(session: &TerminalSession) -> Vec<(Option<String>, String)> {
    // Two phases: snapshot under the store lock, then format each block
    // through the engine (the store and engine locks never nest).
    let items: Vec<(Option<String>, Option<BlockHandle>)> = {
        let store = session.block_store();
        let store = store.lock();

        store
            .items()
            .iter()
            .map(|item| (item.meta.command.clone(), item.handle()))
            .collect()
    };

    items
        .into_iter()
        .map(|(command, handle)| {
            let text = handle
                .and_then(|handle| {
                    let engine = session.engine.lock();
                    engine.block_acquire(handle).and_then(|block| {
                        let rows = block.row_count();

                        if rows == 0 {
                            return Some(String::new());
                        }

                        let last_col = block.cols().saturating_sub(1);

                        block
                            .format_range((0, 0), (rows - 1, last_col), true, true)
                            .ok()
                    })
                })
                .unwrap_or_default();
            (command, text)
        })
        .collect()
}

/// Type `cmd` + Enter and wait for its CommandFinished, up to `timeout`.
/// Bails immediately (false) if the shell process dies — a hung 10-minute
/// wait on a dead session helps nobody.
fn run_command_within(
    session: &TerminalSession,
    all: &mut Vec<HostEvent>,
    cmd: &str,
    timeout: Duration,
) -> bool {
    let finished_before = all
        .iter()
        .filter(|e| matches!(e, HostEvent::CommandFinished { .. }))
        .count();

    session.write_input(cmd.as_bytes());

    thread::sleep(Duration::from_millis(300));

    session.write_input(b"\r");

    wait_for(session, all, timeout, |evs| {
        evs.contains(&HostEvent::Exit)
            || evs
                .iter()
                .filter(|e| matches!(e, HostEvent::CommandFinished { .. }))
                .count()
                > finished_before
    }) && !all.contains(&HostEvent::Exit)
}

fn run_command(session: &TerminalSession, all: &mut Vec<HostEvent>, cmd: &str) -> bool {
    run_command_within(session, all, cmd, Duration::from_secs(15))
}

/// Spawn an integrated session and wait for boundary trust.
fn trusted_session() -> Option<(TerminalSession, Vec<HostEvent>)> {
    let session = match TerminalSession::new(&integration_config(), 1, None) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("skipping: could not spawn powershell.exe: {e:?}");
            return None;
        }
    };

    let mut all = Vec::new();

    assert!(
        wait_for(&session, &mut all, Duration::from_secs(20), |evs| evs
            .contains(&HostEvent::PromptBoundaryTrusted(true)),),
        "integrated prompt never became trusted; events: {all:?}\nscreen:\n{}",
        screen_text(&session)
    );

    Some((session, all))
}

fn last_alt_screen(all: &[HostEvent]) -> Option<bool> {
    all.iter().rev().find_map(|e| match e {
        HostEvent::AltScreen(on) => Some(*on),
        _ => None,
    })
}

/// Bug 1: Ctrl-C while a command sits on the alternate screen (vtebench setup
/// wrote `?1049h`; the per-sample RIS that would leave it never arrives). The
/// next trusted prompt must recover block mode: alt-screen must read false and
/// the next command must produce a frozen block.
#[test]
fn ctrl_c_on_alt_screen_recovers_block_mode() {
    let Some((session, mut all)) = trusted_session() else {
        return;
    };

    // Enter alt screen, then hang like a mid-sample benchmark.
    // Ctrl-C should cut the sleep short; the short sleep bounds the test even
    // if the interrupt is swallowed (the end state is identical either way).
    session.write_input(b"$e=[char]27; [Console]::Write(\"$e[?1049h\"); Start-Sleep 8");
    thread::sleep(Duration::from_millis(300));
    session.write_input(b"\r");
    assert!(
        wait_for(&session, &mut all, Duration::from_secs(10), |evs| {
            last_alt_screen(evs) == Some(true)
        }),
        "alt screen never engaged; events: {all:?}"
    );

    // Ctrl-C, like interrupting vtebench.
    session.write_input(b"\x03");
    assert!(
        wait_for(&session, &mut all, Duration::from_secs(45), |evs| {
            evs.iter()
                .filter(|e| matches!(e, HostEvent::PromptStarted))
                .count()
                >= 2
        }),
        "no prompt returned after Ctrl-C; events: {all:?}\nscreen:\n{}",
        screen_text(&session)
    );

    // Desired: back at a trusted prompt, alt-screen is off again.
    let recovered = wait_for(&session, &mut all, Duration::from_secs(5), |evs| {
        last_alt_screen(evs) == Some(false)
    });
    assert!(
        recovered,
        "alt screen stayed latched ON after Ctrl-C back to prompt — block mode \
         cannot re-engage; events: {all:?}\nscreen:\n{}",
        screen_text(&session)
    );

    // And the next command must produce a block again.
    assert!(
        run_command(&session, &mut all, "echo AFTER_MARKER"),
        "command after Ctrl-C never finished; events: {all:?}"
    );
    let blocks = block_texts(&session);
    assert!(
        blocks
            .iter()
            .any(|(cmd, _)| cmd.as_deref().is_some_and(|c| c.contains("AFTER_MARKER"))),
        "no block recorded for the post-Ctrl-C command; blocks: {blocks:?}"
    );
}

/// Regression: a command that entered AND left the alternate screen before
/// printing its final output (vtebench: `?1049h` in setup, RIS between
/// samples) must still get its whole tail into the block. Failure mode: the
/// prompt's leading `?1049l` makes conhost restore the stale cursor saved at
/// `?1049h`, so `;D` finalizes with the cursor rows above the real output end
/// and the block keeps only the first line(s).
#[test]
fn full_tail_survives_after_alt_screen_roundtrip() {
    let Some((session, mut all)) = trusted_session() else {
        return;
    };

    let cmd = "$e=[char]27; [Console]::Write(\"$e[?1049h\"); \
               [Console]::Write(\"$($e)c\"); \
               1..25 | ForEach-Object { [Console]::WriteLine(\"TAILLINE$_\") }";
    assert!(
        run_command(&session, &mut all, cmd),
        "command never finished; events: {all:?}"
    );
    thread::sleep(Duration::from_millis(500));
    pump(&session, &mut all);

    let blocks = block_texts(&session);
    let all_text = blocks
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for n in [1, 12, 25] {
        assert!(
            all_text.contains(&format!("TAILLINE{n}")),
            "TAILLINE{n} missing from blocks — post-alt-screen tail truncated.\n\
             blocks: {blocks:?}\nscreen:\n{}",
            screen_text(&session)
        );
    }
}

/// Engine-block handle store: with `engine_blocks`
/// enabled, command output freezes into a finished engine block whose HANDLE
/// lands in the store; the content reads back through a `BlockRef`.
#[test]
fn engine_blocks_bridge_freezes_command_output() {
    let mut config = integration_config();
    config.engine_blocks = true;
    let session = match TerminalSession::new(&config, 1, None) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("skipping: could not spawn powershell.exe: {e:?}");
            return;
        }
    };
    let mut all = Vec::new();
    assert!(
        wait_for(&session, &mut all, Duration::from_secs(20), |evs| evs
            .contains(&HostEvent::PromptBoundaryTrusted(true)),),
        "integrated prompt never became trusted; events: {all:?}\nscreen:\n{}",
        screen_text(&session)
    );

    let cmd = "1..5 | ForEach-Object { [Console]::WriteLine(\"ENGINE_BLOCK_ROW_$_\") }";
    assert!(
        run_command(&session, &mut all, cmd),
        "command never finished; events: {all:?}"
    );
    thread::sleep(Duration::from_millis(500));
    pump(&session, &mut all);

    let handle_items = {
        let store = session.block_store();
        let store = store.lock();
        store
            .items()
            .iter()
            .filter(|item| item.handle().is_some())
            .count()
    };
    assert!(
        handle_items > 0,
        "engine-blocks mode must store block handles, not materialized lines; events: {all:?}"
    );

    let blocks = block_texts(&session);
    let hit = blocks.iter().any(|(_, text)| {
        text.contains("ENGINE_BLOCK_ROW_1") && text.contains("ENGINE_BLOCK_ROW_5")
    });
    assert!(
        hit,
        "engine-blocks output missing from frozen blocks.\nblocks: {blocks:?}\nscreen:\n{}\nevents: {all:?}",
        screen_text(&session)
    );
}

/// Bug 2: vtebench's final sample ends with RIS (`ESC c`) and the results table
/// is printed right after it. The results must survive — in the finalized block
/// and/or on the visible screen — not be swallowed by the boundary clear.
#[test]
fn output_after_ris_survives_into_the_block() {
    let Some((session, mut all)) = trusted_session() else {
        return;
    };

    // Emulate one vtebench run tail: alt screen sample, RIS, then results.
    let cmd = "$e=[char]27; [Console]::Write(\"$e[?1049h\"); \
               1..40 | ForEach-Object { [Console]::Write(\"BENCHROW$_`r`n\") }; \
               [Console]::Write(\"$($e)c\"); \
               [Console]::WriteLine(\"RESULTS_MARKER_XYZ\")";
    assert!(
        run_command(&session, &mut all, cmd),
        "RIS command never finished; events: {all:?}"
    );

    // Give the block events a beat to land, then look for the results text.
    thread::sleep(Duration::from_millis(500));
    pump(&session, &mut all);
    let blocks = block_texts(&session);
    let screen = screen_text(&session);
    let in_blocks = blocks
        .iter()
        .any(|(_, text)| text.contains("RESULTS_MARKER_XYZ"));
    let on_screen = screen.contains("RESULTS_MARKER_XYZ");
    eprintln!(
        "[diag] ris_emulation: in_blocks={in_blocks} on_screen={on_screen} \
         alt_screen={:?} blocks(cmd,len)={:?}",
        last_alt_screen(&all),
        blocks
            .iter()
            .map(|(c, t)| (c.clone(), t.len()))
            .collect::<Vec<_>>()
    );
    assert!(
        in_blocks || on_screen,
        "post-RIS results vanished: not in any frozen block and not on screen.\n\
         blocks: {blocks:?}\nscreen:\n{screen}\nevents: {all:?}"
    );
}
