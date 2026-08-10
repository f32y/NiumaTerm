use std::cell;

use crate::prompt_sniffer::{
    ProgressReport, ProgressState, PromptRegion, PromptSniffer, SniffedOsc, parse_sniffed_osc,
};

/// Collect every forwarded (region, bytes) segment from feeding `chunks` in order,
/// reusing one sniffer across chunks (so carry-over across reads is exercised).
fn run(chunks: &[&[u8]]) -> Vec<(PromptRegion, Vec<u8>)> {
    let mut s = PromptSniffer::default();
    let mut out = Vec::new();
    for c in chunks {
        s.feed(c, |r, _, b| out.push((r, b.to_vec())));
    }
    out
}

/// Concatenate what production writes to the engine: segments plus mark bytes.
fn engine_stream(chunks: &[&[u8]]) -> Vec<u8> {
    let mut s = PromptSniffer::default();
    let out = cell::RefCell::new(Vec::new());
    for c in chunks {
        s.feed_hooked(
            c,
            |_, _, b| out.borrow_mut().extend_from_slice(b),
            |m| out.borrow_mut().extend_from_slice(m.bytes),
        );
    }
    out.into_inner()
}

#[test]
fn classifies_full_prompt_command_output_cycle() {
    // Marks are BEL-terminated: ESC]133;A BEL  PS>  ESC]133;B BEL  ls CRLF  …
    let stream = b"\x1b]133;A\x07PS> \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07";
    let segs = run(&[stream]);
    assert_eq!(
        segs,
        vec![
            (PromptRegion::Prompt, b"PS> ".to_vec()),
            (PromptRegion::Command, b"ls\r\n".to_vec()),
            (PromptRegion::Output, b"out\r\n".to_vec()),
        ]
    );
}

#[test]
fn marks_are_forwarded_to_engine() {
    let stream = b"\x1b]133;A\x07PS> \x1b]133;B\x07ls\x1b]133;C\x07out\x1b]133;D\x07";
    assert_eq!(engine_stream(&[stream]), stream);
}

#[test]
fn bytes_before_first_mark_pass_through_as_none() {
    let segs = run(&[b"banner\x1b]133;A\x07prompt"]);
    assert_eq!(segs[0], (PromptRegion::None, b"banner".to_vec()));
    assert_eq!(segs[1], (PromptRegion::Prompt, b"prompt".to_vec()));
}

#[test]
fn accepts_st_terminator() {
    // ST = ESC \ instead of BEL.
    let segs = run(&[b"\x1b]133;A\x1b\\done"]);
    assert_eq!(segs, vec![(PromptRegion::Prompt, b"done".to_vec())]);
}

#[test]
fn ordinary_escape_sequences_are_not_marks() {
    // The OSC 133 mark and an SGR color escape inside output pass through untouched.
    let sgr = b"\x1b]133;C\x07\x1b[31mred\x1b[0m";
    let out = engine_stream(&[sgr]);
    assert_eq!(out, sgr);
}

#[test]
fn mark_split_across_two_reads_is_carried() {
    // The ESC]133;C BEL mark is split mid-sequence across the read boundary.
    let segs = run(&[
        b"\x1b]133;A\x07PS> \x1b]133;B\x07cmd\x1b]13",
        b"3;C\x07output",
    ]);
    assert_eq!(
        segs,
        vec![
            (PromptRegion::Prompt, b"PS> ".to_vec()),
            (PromptRegion::Command, b"cmd".to_vec()),
            (PromptRegion::Output, b"output".to_vec()),
        ]
    );
}

#[test]
fn region_persists_across_reads_without_marks() {
    // After entering Output, a plain second read stays Output.
    let segs = run(&[
        b"\x1b]133;A\x07PS> \x1b]133;B\x07cmd\x1b]133;C\x07first",
        b"second",
    ]);
    assert_eq!(
        segs,
        vec![
            (PromptRegion::Prompt, b"PS> ".to_vec()),
            (PromptRegion::Command, b"cmd".to_vec()),
            (PromptRegion::Output, b"first".to_vec()),
            (PromptRegion::Output, b"second".to_vec()),
        ]
    );
}

fn run_with_trust(chunks: &[&[u8]]) -> Vec<(PromptRegion, bool, Vec<u8>)> {
    let mut s = PromptSniffer::default();
    let mut out = Vec::new();
    for c in chunks {
        s.feed(c, |r, trusted, b| out.push((r, trusted, b.to_vec())));
    }
    out
}

#[test]
fn boundary_trust_follows_ordered_prompt_command_output_cycle() {
    let stream =
        b"\x1b]133;A\x07PS1> \x1b]133;B\x07first\r\n\x1b]133;C\x07out1\r\n\x1b]133;D;0\x07\x1b]133;A\x07PS2> \x1b]133;B\x07second\r\n\x1b]133;C\x07out2\r\n\x1b]133;D;0\x07";
    let segs = run_with_trust(&[stream]);
    assert_eq!(
        segs,
        vec![
            (PromptRegion::Prompt, false, b"PS1> ".to_vec()),
            (PromptRegion::Command, false, b"first\r\n".to_vec()),
            (PromptRegion::Output, false, b"out1\r\n".to_vec()),
            (PromptRegion::Prompt, true, b"PS2> ".to_vec()),
            (PromptRegion::Command, true, b"second\r\n".to_vec()),
            (PromptRegion::Output, true, b"out2\r\n".to_vec()),
        ]
    );
}

#[test]
fn startup_synthetic_cycle_grants_trust_at_first_prompt() {
    // pwsh-integration.ps1 emits an empty ;A;B;C cycle at dot-source time
    // so the first real prompt's leading ;D completes an ordered lifecycle:
    // trust engages before the first Enter.
    let stream = b"banner\r\n\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D\x07\x1b]133;A\x07PS> \x1b]133;B\x07";
    let segs = run_with_trust(&[stream]);
    assert_eq!(
        segs,
        vec![
            (PromptRegion::None, false, b"banner\r\n".to_vec()),
            (PromptRegion::Prompt, true, b"PS> ".to_vec()),
        ]
    );
    assert_eq!(engine_stream(&[stream]), stream);
}

#[test]
fn prompt_without_completed_lifecycle_is_not_trusted() {
    let segs = run_with_trust(&[b"\x1b]133;A\x07PS> \x1b]133;B\x07cmd"]);
    assert_eq!(
        segs,
        vec![
            (PromptRegion::Prompt, false, b"PS> ".to_vec()),
            (PromptRegion::Command, false, b"cmd".to_vec()),
        ]
    );
    let stream = b"\x1b]133;A\x07PS> \x1b]133;B\x07cmd";
    assert_eq!(engine_stream(&[stream]), stream);
}

#[test]
fn malformed_lifecycle_forwards_everything_until_completed_lifecycle() {
    let bad_then_good = b"banner\x1b]133;B\x07visible\x1b]133;C\x07output\x1b]133;D\x07\x1b]133;A\x07PS> \x1b]133;B\x07cmd\x1b]133;C\x07ok";
    assert_eq!(engine_stream(&[bad_then_good]), bad_then_good);
}

#[test]
fn untrusted_stream_forwards_prompt_and_command() {
    let stream = b"\x1b]133;B\x07PS> cmd\x1b]133;C\x07out";
    assert_eq!(engine_stream(&[stream]), stream);
}

#[test]
fn invalid_transition_drops_boundary_trust() {
    let mut s = PromptSniffer::default();
    s.feed(
        b"\x1b]133;A\x07PS> \x1b]133;B\x07\x1b]133;B\x07",
        |_, _, _| {},
    );
    assert!(!s.boundary_trusted());
}

#[test]
fn split_marker_keeps_trust_when_lifecycle_is_valid() {
    let segs = run_with_trust(&[
        b"\x1b]133;A\x07old> \x1b]133;B\x07warm\x1b]133;C\x07up\x1b]133;D\x07",
        b"\x1b]133;A\x07PS> \x1b]13",
        b"3;B\x07cmd\x1b]133;C\x07out",
    ]);
    assert_eq!(
        segs,
        vec![
            (PromptRegion::Prompt, false, b"old> ".to_vec()),
            (PromptRegion::Command, false, b"warm".to_vec()),
            (PromptRegion::Output, false, b"up".to_vec()),
            (PromptRegion::Prompt, true, b"PS> ".to_vec()),
            (PromptRegion::Command, true, b"cmd".to_vec()),
            (PromptRegion::Output, true, b"out".to_vec()),
        ]
    );
}

#[test]
fn malformed_carried_mark_clears_trust_before_forwarding() {
    let mut s = PromptSniffer::default();
    let mut out = Vec::new();
    s.feed(
        b"\x1b]133;A\x07p\x1b]133;B\x07c\x1b]133;C\x07o\x1b]133;D\x07\x1b]133;A\x07",
        |_, _, _| {},
    );
    assert!(s.boundary_trusted());
    s.feed(b"\x1b]133;", |r, trusted, b| {
        if !trusted || matches!(r, PromptRegion::Output | PromptRegion::None) {
            out.extend_from_slice(b);
        }
    });
    s.feed(b"this-is-too-long-for-a-valid-mark", |r, trusted, b| {
        if !trusted || matches!(r, PromptRegion::Output | PromptRegion::None) {
            out.extend_from_slice(b);
        }
    });
    assert!(!s.boundary_trusted());
    assert!(out.starts_with(b"\x1b]133;"));
}

/// A read boundary landing inside an ordinary escape sequence (any chunk
/// ending in a bare ESC / partial OSC introducer — constant under
/// ESC-dense output like vtebench) must forward the bytes untouched and
/// KEEP boundary trust. The vtebench "run to completion, output vanishes"
/// bug: the carried ESC resolved as NotMark but was handled as a
/// boundary glitch, killing trust mid-command with no cycle to recover it.
#[test]
fn ordinary_escape_split_across_reads_keeps_trust() {
    for (a, b_) in [
        (&b"text\x1b"[..], &b"[31mred"[..]),           // CSI split at ESC
        (&b"text\x1b]"[..], &b"0;title\x07"[..]),      // other-OSC split after ]
        (&b"text\x1b]1"[..], &b"0;something\x07"[..]), // split inside "133"-lookalike
    ] {
        let mut s = primed();
        let mut fwd = Vec::new();
        s.feed(a, |_, _, seg| fwd.extend_from_slice(seg));
        s.feed(b_, |_, _, seg| fwd.extend_from_slice(seg));
        assert!(
            s.boundary_trusted(),
            "trust lost on split escape {:?}+{:?}",
            String::from_utf8_lossy(a),
            String::from_utf8_lossy(b_)
        );
        let mut joined = a.to_vec();
        joined.extend_from_slice(b_);
        assert_eq!(fwd, joined, "split escape bytes must forward verbatim");
    }
}

// ---- command-blocks: exit-code extraction + completed-command capture ----

use crate::event::CommandCapture;

/// Feed `input` and collect the completed-command captures the `on_mark` hook
/// delivers (in stream order — multiple completions per read stay ordered).
fn feed_commands(s: &mut PromptSniffer, input: &[u8]) -> Vec<CommandCapture> {
    let mut out = Vec::new();
    s.feed_hooked(
        input,
        |_, _, _| {},
        |mut m| out.extend(m.command_finished.take()),
    );
    out
}

/// A sniffer with boundary trust already established, the way a real session gets
/// it: the ps1's synthetic session-start `;A;B;C` prime closed by the first prompt's
/// `;D` — which itself must produce no command block (the trust-establishing cycle).
fn primed() -> PromptSniffer {
    let mut s = PromptSniffer::default();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07",
    );
    assert!(s.boundary_trusted());
    assert!(
        cmds.is_empty(),
        "the trust-establishing ;D must not produce a block"
    );
    s
}

#[test]
fn escape_split_inside_command_region_keeps_captured_text() {
    // An ordinary escape split by the read boundary inside the ;B→;C echo
    // region: the carried ESC must still land in the command buffer so the
    // capture normalization sees the complete escape sequence and strips
    // it. Dropping the carry leaves a bare "[31m" behind as literal
    // command text ("echo [31mhi").
    let mut s = primed();
    let mut cmds = feed_commands(&mut s, b"\x1b]133;A\x07PS> \x1b]133;B\x07echo \x1b");
    cmds.extend(feed_commands(
        &mut s,
        b"[31mhi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07",
    ));
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].command, "echo hi");
}

#[test]
fn trusted_cycle_produces_block_with_exit_code_and_timing() {
    let mut s = primed();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07",
    );
    assert_eq!(cmds.len(), 1, "trusted cycle yields exactly one block");
    let cmd = &cmds[0];
    assert_eq!(cmd.command, "echo hi");
    assert_eq!(cmd.exit_code, Some(0));
    assert!(cmd.started_at <= cmd.ended_at);
}

#[test]
fn failing_exit_code_with_st_terminator_is_extracted() {
    let mut s = primed();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07cmd /c exit 3\r\n\x1b]133;C\x07\x1b]133;D;3\x1b\\",
    );
    assert_eq!(cmds[0].exit_code, Some(3));
}

#[test]
fn negative_exit_code_is_parsed() {
    // Windows native failures commonly surface as negative NTSTATUS-style codes.
    let mut s = primed();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07x\r\n\x1b]133;C\x07\x1b]133;D;-1073741510\x07",
    );
    assert_eq!(cmds[0].exit_code, Some(-1073741510));
}

#[test]
fn bare_d_records_unknown_exit_code() {
    // A foreign integration emitting a bare ;D still records the block.
    let mut s = primed();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D\x07",
    );
    assert_eq!(cmds.len(), 1, "block recorded without an exit code");
    assert_eq!(cmds[0].exit_code, None);
    assert_eq!(cmds[0].command, "ls");
}

#[test]
fn exit_code_mark_split_across_reads_is_carried() {
    let mut s = primed();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07x\r\n\x1b]133;C\x07out\x1b]13",
    );
    assert!(cmds.is_empty(), "mark incomplete — no block yet");
    let cmds = feed_commands(&mut s, b"3;D;42\x07");
    assert_eq!(cmds[0].exit_code, Some(42));
}

#[test]
fn two_completions_in_one_read_stay_ordered() {
    // A pasted multiline script can complete several commands in one PTY read;
    // the hook delivers each block at its own ;D in stream order.
    let mut s = primed();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07first\r\n\x1b]133;C\x07\x1b]133;D;0\x07\
\x1b]133;A\x07> \x1b]133;B\x07second\r\n\x1b]133;C\x07\x1b]133;D;1\x07",
    );
    assert_eq!(cmds.len(), 2);
    assert_eq!(
        (cmds[0].command.as_str(), cmds[0].exit_code),
        ("first", Some(0))
    );
    assert_eq!(
        (cmds[1].command.as_str(), cmds[1].exit_code),
        ("second", Some(1))
    );
}

#[test]
fn untrusted_or_partial_cycle_produces_no_block() {
    // Lifecycle starting at ;B (out of order): untrusted, no block.
    let mut s = PromptSniffer::default();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;B\x07cmd\x1b]133;C\x07out\x1b]133;D;0\x07",
    );
    assert!(cmds.is_empty());

    // A first ordered cycle with no prior trust: its ;D establishes trust but the
    // cycle itself is skipped (Decision 5 — trust-recovery command is not recorded).
    let mut s = PromptSniffer::default();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07real\r\n\x1b]133;C\x07out\x1b]133;D;0\x07",
    );
    assert!(s.boundary_trusted());
    assert!(cmds.is_empty());
}

#[test]
fn empty_or_whitespace_command_produces_no_block() {
    let mut s = primed();
    // Enter at an empty prompt: the echo region holds only the CRLF.
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07\r\n\x1b]133;C\x07\x1b]133;D;0\x07",
    );
    assert!(cmds.is_empty());
}

#[test]
fn command_text_is_stripped_of_sgr_and_controls() {
    // PSReadLine colorizes the echo; the recorded command must be plain text.
    let mut s = primed();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07\x1b[93mgit\x1b[0m status\r\n\x1b]133;C\x07\x1b]133;D;0\x07",
    );
    assert_eq!(cmds[0].command, "git status");
}

#[test]
fn command_echo_redraws_converge_to_final_line() {
    // PSReadLine redraws the input per keystroke and ConPTY reprojects each redraw
    // at absolute columns; the emulation must overwrite in place, not concatenate.
    let mut s = primed();
    let cmds = feed_commands(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07\
\x1b[1;9H\x1b[K\x1b[93me\x1b[0m\
\x1b[1;9H\x1b[K\x1b[93mecho\x1b[0m hi\
\x1b[1;9H\x1b[K\x1b[93mecho\x1b[0m hi2\r\n\
\x1b]133;C\x07\x1b]133;D;0\x07",
    );
    assert_eq!(cmds[0].command, "echo hi2");
}

// ---- command lifecycle: mark hook edges + mark bytes ----

/// The `on_mark` hook fires at each mark's exact stream position: after the bytes
/// before it are forwarded, before any byte after it — with the mark's raw bytes,
/// the ;A prompt-start edge, and the trusted-;C command-start payload.
#[test]
fn mark_hook_fires_in_stream_order_with_edges() {
    let mut s = primed();
    let log = cell::RefCell::new(Vec::<String>::new());
    s.feed_hooked(
        b"\x1b]133;A\x07p>\x1b]133;B\x07cmd\x1b]133;C\x07OUT",
        |_, _, seg| {
            log.borrow_mut()
                .push(format!("seg:{}", String::from_utf8_lossy(seg)));
        },
        |m| {
            let mut tags = Vec::new();
            if m.prompt_started {
                tags.push("A".to_string());
            }
            if let Some(start) = &m.command_started {
                tags.push(format!("C:{}", start.command));
            }
            if m.command_finished.is_some() {
                tags.push("D".to_string());
            }
            log.borrow_mut().push(format!("mark[{}]", tags.join(",")));
        },
    );
    assert_eq!(
        log.into_inner(),
        vec![
            "mark[A]",
            "seg:p>",
            "mark[]", // ;B — no edge payload
            "seg:cmd",
            "mark[C:cmd]",
            "seg:OUT",
        ]
    );
}

/// Mark bytes are handed to the hook verbatim (the production caller writes them
/// to the engine so its per-row semantic tags populate), including a mark that
/// was split across reads and resolved from the carry buffer.
#[test]
fn mark_hook_receives_raw_mark_bytes_including_carry() {
    let mut s = PromptSniffer::default();
    let marks = cell::RefCell::new(Vec::<Vec<u8>>::new());
    let feed = |s: &mut PromptSniffer, input: &[u8]| {
        s.feed_hooked(
            input,
            |_, _, _| {},
            |m| marks.borrow_mut().push(m.bytes.to_vec()),
        );
    };
    feed(&mut s, b"\x1b]133;A\x07p\x1b]13");
    feed(&mut s, b"3;B\x07");
    let marks = marks.into_inner();
    assert_eq!(
        marks,
        vec![b"\x1b]133;A\x07".to_vec(), b"\x1b]133;B\x07".to_vec()]
    );
}

/// The trust-establishing ;C (synthetic prime / recovery cycle) is untrusted at
/// command start, so no in-flight payload is produced for it; the first real
/// command produces exactly one.
#[test]
fn command_started_only_for_trusted_nonempty_commands() {
    let mut s = PromptSniffer::default();
    let starts = cell::RefCell::new(Vec::<String>::new());
    let feed = |s: &mut PromptSniffer, input: &[u8]| {
        s.feed_hooked(
            input,
            |_, _, _| {},
            |mut m| {
                if let Some(start) = m.command_started.take() {
                    starts.borrow_mut().push(start.command);
                }
            },
        );
    };
    // Synthetic prime: untrusted at its ;C, empty command — no start.
    feed(
        &mut s,
        b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07",
    );
    assert!(starts.borrow().is_empty());
    // First real command: trusted, non-empty — one start with the echo text.
    feed(
        &mut s,
        b"\x1b]133;A\x07> \x1b]133;B\x07sleep 5\r\n\x1b]133;C\x07",
    );
    assert_eq!(starts.borrow().as_slice(), ["sleep 5".to_string()]);
    // Empty Enter afterwards: no start.
    feed(
        &mut s,
        b"\x1b]133;D;0\x07\x1b]133;A\x07> \x1b]133;B\x07\r\n\x1b]133;C\x07",
    );
    assert_eq!(starts.borrow().len(), 1);
}

/// The parsed state/percentage is what the tab strip draws, so a report
/// that only resolves to "active" is not enough.
#[test]
fn osc_progress_carries_state_and_percentage() {
    let report = |stream: &[u8]| match parse_sniffed_osc(stream) {
        SniffedOsc::Progress { report, .. } => report,
        _ => panic!("expected a progress report"),
    };

    assert_eq!(
        report(b"\x1b]9;4;1;40\x07"),
        ProgressReport {
            state: ProgressState::Set,
            progress: Some(40),
        }
    );
    // ST-terminated, and a percentage past 100 clamps.
    assert_eq!(
        report(b"\x1b]9;4;2;250\x1b\\"),
        ProgressReport {
            state: ProgressState::Error,
            progress: Some(100),
        }
    );
    // Indeterminate carries no meaningful percentage.
    assert_eq!(
        report(b"\x1b]9;4;3;0\x07"),
        ProgressReport {
            state: ProgressState::Indeterminate,
            progress: Some(0),
        }
    );
    assert_eq!(
        report(b"\x1b]9;4;0;\x07"),
        ProgressReport {
            state: ProgressState::Remove,
            progress: None,
        }
    );
    assert!(matches!(
        parse_sniffed_osc(b"\x1b]9;4;7;10\x07"),
        SniffedOsc::ProgressMalformed
    ));
}

#[test]
fn right_prompt_marker_stays_in_prompt_region() {
    let stream = b"\x1b]133;A\x07left>\x1b]133;P;k=r\x07right\x1b]133;B\x07cmd\x1b]133;C\x07out";
    let segs = run_with_trust(&[stream]);
    assert_eq!(
        segs,
        vec![
            (PromptRegion::Prompt, false, b"left>".to_vec()),
            (PromptRegion::Prompt, false, b"right".to_vec()),
            (PromptRegion::Command, false, b"cmd".to_vec()),
            (PromptRegion::Output, false, b"out".to_vec()),
        ]
    );
}
