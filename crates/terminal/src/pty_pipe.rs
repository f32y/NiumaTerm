//! The PTY pipe: the per-session blocking worker ([`PtyPipe`]) that pumps bytes
//! between ConPTY and the Ghostty engine. It reads PTY output into
//! `engine.write_vt` + the render buffer, flushes queued UI input back to the
//! PTY, and hosts the on-read interceptors: the OSC 133 prompt sniffer
//! (capture / lifecycle events) and ConPTY resize-echo realignment. It runs on
//! its own blocking thread; separate engine and render-buffer locks keep parsing
//! from blocking paint.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::sync::{Arc, mpsc};
use std::thread::{Builder, JoinHandle};

use nmt_platform::{Events, Interest, Poll, Token, Waker};
use parking_lot::FairMutex;
use tracing::error;

use crate::event::{
    CommandCapture, CommandStart, EventListener, Msg, MsgSender, TerminalEvent, WindowId,
};
use crate::ghostty::{GhosttyTerminal, mode};
use crate::render_buffer::RenderBuffer;

/// Like `thread::spawn`, but with a `name` argument.
pub fn spawn_named<F, T, S>(name: S, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    S: Into<String>,
{
    Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("thread spawn works")
}

/// Reserved `Poll` token for the loop's `Waker`. PTY source tokens start above it.
const WAKER_TOKEN: Token = Token(0);

const READ_BUFFER_SIZE: usize = 0x10_0000;
/// Max bytes to read from the PTY while the terminal is locked.
const MAX_LOCKED_READ: usize = u16::MAX as usize;
const BLOCK_BOUNDARY_CLEAR: &[u8] = b"\x1b[2J\x1b[3J\x1b[H";

/// Engine-blocks mode: the engine's live finished-block list, oldest first,
/// with current row counts — the payload of
/// [`crate::event::BlockEvent::EngineBlocksSync`]. Cheap FFI walk; called
/// under the engine lock on the PTY thread after finish/resize.
fn engine_blocks_live_list(engine: &GhosttyTerminal) -> Vec<(crate::ghostty::BlockHandle, usize)> {
    let count = engine.block_count();
    let mut live = Vec::with_capacity(count);
    for index in 0..count {
        if let Some(handle) = engine.block_at(index) {
            live.push((handle, engine.block_row_count(handle).unwrap_or(0)));
        }
    }
    live
}

/// Min interval between full-viewport `snapshot()` + render-buffer readbacks
/// while PTY input is saturated (a batch hit `MAX_LOCKED_READ` with more data
/// pending). The readback is a constant-cost per-cell FFI walk (~1ms at 220×55)
/// that used to run once per batch and dominated the vtebench cell benchmarks.
/// The renderer samples
/// the render buffer at most once per display frame, so under saturation one
/// readback per frame is enough. 5ms sits below one 144 Hz frame (6.94ms), so
/// high-refresh displays still get a fresh grid every frame; plumb the real
/// monitor refresh rate here if 240 Hz+ becomes a target. A caught-up read
/// (interactive echo) always snapshots — this only coalesces under saturation.
const SNAPSHOT_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);
/// Match Windows Terminal's upper bound so a missing DEC 2026 reset cannot
/// leave the last committed frame visible indefinitely.
const SYNC_OUTPUT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

fn is_conpty_resize_echo_input(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes.iter().all(|b| *b >= 0x20 && *b != 0x7f && *b != 0x1b)
}

/// Scan `bytes` for CSI sequences with digit/semicolon params — the one CSI
/// parse every ConPTY resize-echo helper shares. Calls `f(start, final_idx)`
/// per sequence: `params = &bytes[start + 2..final_idx]`, final byte =
/// `bytes[final_idx]`. Cold path (resize windows only).
fn for_each_csi(bytes: &[u8], mut f: impl FnMut(usize, usize)) {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut fin = i + 2;
            while fin < bytes.len() && (bytes[fin].is_ascii_digit() || bytes[fin] == b';') {
                fin += 1;
            }
            if fin < bytes.len() {
                f(i, fin);
            }
            i = fin.saturating_add(1);
        } else {
            i += 1;
        }
    }
}

/// Parse `row;col` CUP params; either half is `None` when absent/unparsable.
fn cup_row_col(params: &[u8]) -> (Option<u16>, Option<u16>) {
    let parse = |s: &[u8]| std::str::from_utf8(s).ok().and_then(|s| s.parse().ok());
    match params.iter().position(|b| *b == b';') {
        Some(i) => (parse(&params[..i]), parse(&params[i + 1..])),
        None => (parse(params), None),
    }
}

fn is_cup(fin: u8) -> bool {
    fin == b'H' || fin == b'f'
}

fn rewrite_conpty_resize_echo_cup_rows(bytes: &[u8], target_row_1based: u16) -> Option<Vec<u8>> {
    if target_row_1based == 0 {
        return None;
    }

    // ConPTY positions the cursor with the LAST CUP in the redraw. Align *that* row
    // to ghostty's cursor row and shift every CUP by the same delta, preserving the
    // multi-row structure of a wrapped input redraw. (Collapsing every CUP onto a
    // single row corrupts wrapped input: at a narrow width PSReadLine redraws the
    // input across several rows — e.g. `[10;22H…xxx…[11;6H` — and forcing the first
    // CUP to the cursor row makes the content re-wrap/scroll and accumulate.) When
    // ConPTY's cursor row already matches (delta 0), there is nothing to fix.
    let conpty_cursor_row = last_cup_row(bytes)?;
    let delta = target_row_1based as i32 - conpty_cursor_row as i32;
    if delta == 0 {
        return None;
    }

    let mut out = Vec::with_capacity(bytes.len().saturating_add(8));
    let mut copied = 0usize;
    for_each_csi(bytes, |start, fin| {
        if !is_cup(bytes[fin]) {
            return;
        }
        let params = &bytes[start + 2..fin];
        let (Some(row), _) = cup_row_col(params) else {
            return;
        };
        let new_row = (row as i32 + delta).max(1) as u16;
        if new_row == row {
            return;
        }
        let row_end = params
            .iter()
            .position(|b| *b == b';')
            .unwrap_or(params.len());
        out.extend_from_slice(&bytes[copied..start]);
        out.extend_from_slice(b"\x1b[");
        out.extend_from_slice(new_row.to_string().as_bytes());
        out.extend_from_slice(&params[row_end..]);
        out.push(bytes[fin]);
        copied = fin + 1;
    });
    if copied == 0 {
        return None;
    }
    out.extend_from_slice(&bytes[copied..]);
    Some(out)
}

/// The row of the LAST CUP (`CSI row;col H/f`) in `bytes` — ConPTY's resulting
/// cursor row after a redraw. `None` if there is no CUP with an explicit row.
fn last_cup_row(bytes: &[u8]) -> Option<u16> {
    let mut last = None;
    for_each_csi(bytes, |start, fin| {
        if is_cup(bytes[fin]) {
            if let (Some(r), _) = cup_row_col(&bytes[start + 2..fin]) {
                last = Some(r);
            }
        }
    });
    last
}

/// The maximum row and column (both 1-based) addressed by any CUP in `bytes`. Used to
/// sanity-check that a ConPTY repaint fits the latched resize size before SU
/// realignment. `(0, 0)` if there is no CUP.
fn max_cup_row_col(bytes: &[u8]) -> (u16, u16) {
    let (mut max_row, mut max_col) = (0u16, 0u16);
    for_each_csi(bytes, |start, fin| {
        if is_cup(bytes[fin]) {
            let (row, col) = cup_row_col(&bytes[start + 2..fin]);
            max_row = max_row.max(row.unwrap_or(0));
            max_col = max_col.max(col.unwrap_or(0));
        }
    });
    (max_row, max_col)
}

/// Rows to Scroll-Up to realign ghostty's prompt with ConPTY's, plus the
/// resulting prompt row (`R_conpty`, 0-based) for the post-SU assertion. `None` when the
/// repaint has no CUP, fails the resize correspondence pre-check, or `N <= 0`.
///
/// `r_ghostty` is the latched `active_cursor_row()` (0-based); `R_conpty` is the
/// repaint's last CUP row (0-based) — both the cursor row, kept anchor-matched.
fn su_realign_count(
    repaint: &[u8],
    r_ghostty: u16,
    latched_cols: u16,
    latched_rows: u16,
    engine_cols: u16,
    engine_rows: u16,
) -> Option<(u16, u16)> {
    let r_conpty = last_cup_row(repaint)?.saturating_sub(1);
    let (max_row, max_col) = max_cup_row_col(repaint);
    let fits = engine_cols == latched_cols
        && engine_rows == latched_rows
        && max_row <= latched_rows
        && max_col <= latched_cols;
    if !fits || r_ghostty <= r_conpty {
        return None;
    }
    let n = (r_ghostty - r_conpty).min(r_ghostty);
    (n > 0).then_some((n, r_conpty))
}

fn first_cup_row(bytes: &[u8]) -> Option<u16> {
    let mut first = None;
    for_each_csi(bytes, |start, fin| {
        if first.is_none() && is_cup(bytes[fin]) {
            first = cup_row_col(&bytes[start + 2..fin]).0;
        }
    });
    first
}

fn contains_csi_erase_display(bytes: &[u8]) -> bool {
    let mut found = false;
    for_each_csi(bytes, |_, fin| found |= bytes[fin] == b'J');
    found
}

fn is_conpty_resize_repaint(bytes: &[u8], target_row_1based: u16) -> bool {
    if target_row_1based == 0 || bytes.is_empty() || bytes.len() > 512 {
        return false;
    }
    if bytes.iter().any(|b| *b == b'\r' || *b == b'\n') {
        return false;
    }
    if !contains_csi_erase_display(bytes) {
        return false;
    }

    matches!(first_cup_row(bytes), Some(row) if row != target_row_1based)
}

/// Escape raw PTY bytes for human-readable vt_trace output.
fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            b'\x1b' => out.push_str("<ESC>"),
            b'\r' => out.push_str("<CR>"),
            b'\n' => out.push_str("<LF>"),
            0x20..=0x7e => out.push(b as char),
            _ => {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("\\x{b:02x}"));
            }
        }
    }
    out
}

// ---- OSC 133 shell-integration prompt sniffer ----
//
// Recognizes FinalTerm OSC 133 marks (`ESC ] 133 ; A/B/C/D <term>`) in the PTY byte
// stream to track the prompt/command/output region for capture while forwarding the PTY
// stream unchanged. Hot-path rule
// Keep the PTY hot path cheap: memchr-skip to ESC, never a per-byte walk; forward
// by sub-slice with no filtered-buffer allocation; a split mark is held in a fixed inline
// buffer, not a heap Vec.

/// FinalTerm OSC 133 region. `None` = before the first prompt (banner/MOTD) or after a
/// command ends (`;D`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum PromptRegion {
    #[default]
    None,
    Prompt,  // ;A → ;B  (the shell prompt)
    Command, // ;B → ;C  (the echoed command line)
    Output,  // ;C → ;D  (command output)
}

/// Whether the parsed OSC 133 lifecycle is trusted for prompt hiding.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum ShellBoundaryTrust {
    #[default]
    Untrusted,
    Trusted,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum ShellLifecycleProgress {
    #[default]
    AwaitPrompt,
    InPrompt,
    InCommand,
    InOutput,
}

/// `ESC ] 1 3 3 ;` - the OSC 133 introducer.
const OSC133_PREFIX: &[u8] = b"\x1b]133;";
/// Max bytes of one mark we buffer/scan (`ESC]133;D;<exit>ST` is far shorter). A malformed
/// mark longer than this resyncs as ordinary bytes; also bounds the inline carry buffer.
const OSC133_MAX: usize = 32;

enum Osc133 {
    /// A complete mark: consume `len` bytes; `next` is the region to switch to (`None` =
    /// a recognized 133 mark with no transition, e.g. `;P` right-prompt). `exit` is the
    /// command exit code carried by a `;D;<code>` mark (`None` for a bare `;D`).
    Mark {
        len: usize,
        sub: u8,
        exit: Option<i32>,
        next: Option<PromptRegion>,
    },
    /// Looks like the start of a 133 mark but the slice ends before the terminator — carry.
    Incomplete,
    /// Not a 133 mark; the ESC is an ordinary terminal byte.
    NotMark,
    /// Starts like OSC 133 but is malformed or too long.
    Malformed,
}

fn region_for(sub: u8) -> Option<PromptRegion> {
    match sub {
        b'A' => Some(PromptRegion::Prompt),
        b'B' => Some(PromptRegion::Command),
        b'C' => Some(PromptRegion::Output),
        b'D' => Some(PromptRegion::None),
        _ => None, // recognized mark, no region transition (e.g. ;P)
    }
}

/// The `;D` mark's exit-code argument: `arg` is the bytes between the subcommand and the
/// terminator (`;42` for `ESC]133;D;42<BEL>`; empty for a bare `;D`).
fn parse_exit_code(arg: &[u8]) -> Option<i32> {
    let digits = arg.strip_prefix(b";")?;
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// Parse a potential OSC 133 mark at `s` (caller guarantees `s[0] == 0x1b`).
fn parse_osc133(s: &[u8]) -> Osc133 {
    let pn = s.len().min(OSC133_PREFIX.len());
    if s[..pn] != OSC133_PREFIX[..pn] {
        return Osc133::NotMark;
    }
    if s.len() <= OSC133_PREFIX.len() {
        return Osc133::Incomplete; // prefix matched so far; need the subcommand + terminator
    }
    let sub = s[OSC133_PREFIX.len()];
    let arg_start = OSC133_PREFIX.len() + 1;
    let exit_at = |term: usize| {
        (sub == b'D')
            .then(|| parse_exit_code(&s[arg_start..term]))
            .flatten()
    };
    // Find the terminator: BEL (0x07) or ST (ESC \), bounded by OSC133_MAX.
    let end = s.len().min(OSC133_MAX);
    let mut i = OSC133_PREFIX.len();
    while i < end {
        match s[i] {
            0x07 => {
                return Osc133::Mark {
                    len: i + 1,
                    sub,
                    exit: exit_at(i),
                    next: region_for(sub),
                };
            }
            0x1b if i + 1 < s.len() && s[i + 1] == 0x5c => {
                return Osc133::Mark {
                    len: i + 2,
                    sub,
                    exit: exit_at(i),
                    next: region_for(sub),
                };
            }
            0x1b if i + 1 == s.len() => return Osc133::Incomplete, // maybe a split ST
            0x1b => return Osc133::Malformed,                      // ESC mid-mark that isn't ST
            _ => i += 1,
        }
    }
    if s.len() < OSC133_MAX {
        Osc133::Incomplete // a terminator may still arrive next read
    } else {
        Osc133::Malformed // too long, resync on the ESC
    }
}

/// Render the command-echo region's bytes (`;B`→`;C`) into the final command line
/// (command-blocks). A plain control-strip is not enough here: PSReadLine redraws the
/// input on nearly every keystroke (syntax highlighting) and ConPTY reprojects each
/// redraw at absolute columns, so concatenating printables duplicates the line once per
/// redraw. Instead emulate a single line: printables write at the cursor column
/// (overwriting earlier redraws), CR/CUP/CHA/CUF/CUB move the cursor, EL/ECH erase.
/// Known ceiling: rows are collapsed onto the one line, so a wrapped multi-row command
/// can self-overwrite; engine-grid readback (per-row SEMANTIC_PROMPT tags) is the
/// upgrade path if that fidelity matters.
fn render_command_echo(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let chars: Vec<char> = text.chars().collect();
    let mut line: Vec<char> = Vec::new();
    let mut col = 0usize;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\x1b' => {
                i += 1;
                match chars.get(i) {
                    Some('[') => {
                        // CSI: ESC [ params… final byte (0x40..=0x7e).
                        i += 1;
                        let params_start = i;
                        while i < chars.len() && !('\x40'..='\x7e').contains(&chars[i]) {
                            i += 1;
                        }
                        let Some(&fin) = chars.get(i) else { break };
                        let params: String = chars[params_start..i].iter().collect();
                        i += 1;
                        let nth = |n: usize, def: usize| {
                            params
                                .split(';')
                                .nth(n)
                                .and_then(|s| s.parse::<usize>().ok())
                                .unwrap_or(def)
                        };
                        match fin {
                            // CUP row;col — only the column matters on our one line.
                            'H' | 'f' => col = nth(1, 1).saturating_sub(1),
                            'G' => col = nth(0, 1).saturating_sub(1), // CHA
                            'C' => col += nth(0, 1).max(1),           // CUF
                            'D' => col = col.saturating_sub(nth(0, 1).max(1)), // CUB
                            'K' => match nth(0, 0) {
                                0 => line.truncate(col), // EL to end
                                2 => line.clear(),       // EL whole line
                                _ => {
                                    for c in line.iter_mut().take(col) {
                                        *c = ' '; // EL to start
                                    }
                                }
                            },
                            'X' => {
                                // ECH n: blank n cells at the cursor.
                                let end = (col + nth(0, 1).max(1)).min(line.len());
                                for c in line.iter_mut().take(end).skip(col) {
                                    *c = ' ';
                                }
                            }
                            _ => {} // SGR and the rest have no text effect
                        }
                    }
                    Some(']') => {
                        // OSC … (BEL or ST)
                        i += 1;
                        while i < chars.len() {
                            if chars[i] == '\x07' {
                                i += 1;
                                break;
                            }
                            if chars[i] == '\x1b' && chars.get(i + 1) == Some(&'\\') {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    _ => i += 1, // ESC + single intermediate/final
                }
            }
            '\r' => {
                col = 0;
                i += 1;
            }
            '\x08' => {
                col = col.saturating_sub(1);
                i += 1;
            }
            c if c >= ' ' => {
                while line.len() < col {
                    line.push(' ');
                }
                if col < line.len() {
                    line[col] = c;
                } else {
                    line.push(c);
                }
                col += 1;
                i += 1;
            }
            _ => i += 1, // LF (row collapse), TAB, BEL, other controls — drop
        }
    }
    line.into_iter().collect::<String>().trim().to_string()
}

/// Tracks the OSC 133 region across PTY reads. A mark split across a read boundary is held
/// in `carry` while the PTY stream is forwarded byte-for-byte to the engine.
#[derive(Default)]
pub(crate) struct PromptSniffer {
    region: PromptRegion,
    boundary_trust: ShellBoundaryTrust,
    boundary_trust_changed: Option<bool>,
    lifecycle: ShellLifecycleProgress,
    carry: [u8; OSC133_MAX],
    carry_len: usize,
    /// Accumulates the current command-echo region's raw bytes (`;B`→`;C`), mirroring
    /// the shell lifecycle. Cleared at each `;B`; never grows on the output hot path.
    command_buf: Vec<u8>,
    /// When command output started (`;C`) — the block's `started_at`.
    command_started_at: Option<std::time::SystemTime>,
    /// The command echo rendered once at `;C` (`render_command_echo`); reused by the
    /// `;D` capture so the echo emulation runs once per command.
    current_command: String,
    /// Edge flags/payloads set by `apply` and drained by `feed_hooked` into the
    /// `on_mark` hook at the mark's exact stream position.
    prompt_start_edge: bool,
    command_started_edge: bool,
    command_finished: Option<CommandCapture>,
    /// `;K` — our private "history cleared" mark, emitted by the integration
    /// script's Clear-Host wrapper (the boundary protocol keeps the engine's
    /// scrollback empty, so a user `clear` is invisible to the
    /// history-collapse heuristic and must be announced in-band).
    history_cleared_edge: bool,
}

/// A recognized OSC 133 mark, handed to `feed_hooked`'s `on_mark` hook at its exact
/// stream position — every byte before it already forwarded, none after. The caller
/// forwards `bytes` to the engine and latches launch cwd.
pub(crate) struct SnifferMark<'a> {
    /// Raw mark bytes (`ESC ] 133 ; …` through its terminator) for engine forwarding.
    pub bytes: &'a [u8],
    pub trusted: bool,
    /// Valid transition into the Prompt region (`;A`).
    pub prompt_started: bool,
    /// A trusted command began output (`;C`, non-empty echo): the in-flight block.
    /// `cwd` is `None` from the sniffer; the caller fills it.
    pub command_started: Option<CommandStart>,
    /// A trusted command completed (`;D`): the capture missing `cwd`, which the caller
    /// fills from its latch.
    pub command_finished: Option<CommandCapture>,
    /// The user cleared the terminal (`;K` from the Clear-Host wrapper): the
    /// frozen history must drop with the screen.
    pub history_cleared: bool,
}

impl PromptSniffer {
    /// Feed one PTY read. Calls `forward(region, bytes)` for each run of ordinary (non-mark)
    /// bytes, tagged with the region they belong to; OSC 133 marks advance the region and
    /// are dropped (no `on_mark` hook). It uses `memchr` to skip to ESC rather than
    /// walking every byte, keeping the PTY hot path allocation-free. Production feeds
    /// through `feed_hooked` (which forwards marks to the engine); this hook-less form is the test
    /// harness's entry point.
    #[cfg(test)]
    fn feed(&mut self, input: &[u8], forward: impl FnMut(PromptRegion, bool, &[u8])) {
        self.feed_hooked(input, forward, |_| {});
    }

    /// `feed` with an `on_mark` hook, fired for every recognized OSC 133 mark at its exact
    /// stream position — after every byte before it has been forwarded and before any byte
    /// after it. The caller forwards the mark to the engine (row semantic tags) and latches
    /// the launch cwd at `;C` there: the ps1 emits the NEXT prompt's OSC 7 just before
    /// `;D`, so any later cwd latch would mislabel every `cd`.
    fn feed_hooked(
        &mut self,
        input: &[u8],
        mut forward: impl FnMut(PromptRegion, bool, &[u8]),
        mut on_mark: impl FnMut(SnifferMark),
    ) {
        let mut pos = 0usize;

        // Resolve a mark carried from the previous read, if any.
        if self.carry_len > 0 {
            let cl = self.carry_len;
            let take = (OSC133_MAX - cl).min(input.len());
            let mut tmp = [0u8; OSC133_MAX];
            tmp[..cl].copy_from_slice(&self.carry[..cl]);
            tmp[cl..cl + take].copy_from_slice(&input[..take]);
            match parse_osc133(&tmp[..cl + take]) {
                Osc133::Mark {
                    len,
                    sub,
                    exit,
                    next,
                } => {
                    self.apply(sub, next, exit);
                    let mark = self.drain_mark(&tmp[..len]);
                    on_mark(mark);
                    self.carry_len = 0;
                    pos = len - cl; // skip the input portion of the mark
                }
                Osc133::Incomplete if cl + take < OSC133_MAX => {
                    self.carry[cl..cl + take].copy_from_slice(&input[..take]);
                    self.carry_len = cl + take;
                    return; // still incomplete — wait for the next read
                }
                Osc133::NotMark => {
                    // The carried ESC resolved to an ordinary escape split across
                    // reads (any chunk ending in "\x1b" or "\x1b]…" lands here —
                    // constant under ESC-dense output like vtebench). Forward the
                    // carried bytes and rescan the new input; NOT a boundary
                    // glitch, so trust is untouched.
                    let mut tmp2 = [0u8; OSC133_MAX];
                    tmp2[..cl].copy_from_slice(&self.carry[..cl]);
                    forward(self.region, self.boundary_trusted(), &tmp2[..cl]);
                    self.carry_len = 0;
                }
                _ => {
                    // Malformed/maxed carry: forward the carried bytes as native output and
                    // reprocess the new input under cleared trust.
                    let mut tmp2 = [0u8; OSC133_MAX];
                    tmp2[..cl].copy_from_slice(&self.carry[..cl]);
                    self.reset_boundary_state();
                    forward(self.region, self.boundary_trusted(), &tmp2[..cl]);
                    self.carry_len = 0;
                }
            }
        }

        // Main scan: memchr to the next ESC, classify, forward the run before it.
        let mut seg_start = pos;
        while pos < input.len() {
            match memchr::memchr(0x1b, &input[pos..]) {
                None => break,
                Some(off) => {
                    let esc = pos + off;
                    match parse_osc133(&input[esc..]) {
                        Osc133::Mark {
                            len,
                            sub,
                            exit,
                            next,
                        } => {
                            if esc > seg_start {
                                self.pre_forward(&input[seg_start..esc]);
                                forward(
                                    self.region,
                                    self.boundary_trusted(),
                                    &input[seg_start..esc],
                                );
                            }
                            self.apply(sub, next, exit);
                            let mark = self.drain_mark(&input[esc..esc + len]);
                            on_mark(mark);
                            pos = esc + len;
                            seg_start = pos;
                        }
                        Osc133::Incomplete => {
                            if esc > seg_start {
                                self.pre_forward(&input[seg_start..esc]);
                                forward(
                                    self.region,
                                    self.boundary_trusted(),
                                    &input[seg_start..esc],
                                );
                            }
                            let tail = &input[esc..];
                            let n = tail.len().min(OSC133_MAX);
                            self.carry[..n].copy_from_slice(&tail[..n]);
                            self.carry_len = n;
                            return;
                        }
                        Osc133::NotMark => pos = esc + 1, // ordinary ESC; keep in the run
                        Osc133::Malformed => {
                            if esc > seg_start {
                                self.pre_forward(&input[seg_start..esc]);
                                forward(
                                    self.region,
                                    self.boundary_trusted(),
                                    &input[seg_start..esc],
                                );
                            }
                            self.reset_boundary_state();
                            pos = esc + 1;
                            seg_start = esc;
                        }
                    }
                }
            }
        }
        if input.len() > seg_start {
            self.pre_forward(&input[seg_start..]);
            forward(self.region, self.boundary_trusted(), &input[seg_start..]);
        }
    }

    /// Build the `on_mark` payload for a just-applied mark, draining the edge
    /// flags/payloads `apply` set. `bytes` are the mark's raw bytes.
    fn drain_mark<'a>(&mut self, bytes: &'a [u8]) -> SnifferMark<'a> {
        SnifferMark {
            bytes,
            trusted: self.boundary_trusted(),
            prompt_started: std::mem::take(&mut self.prompt_start_edge),
            command_started: std::mem::take(&mut self.command_started_edge).then(|| CommandStart {
                seq: 0, // stamped by the mark-closure caller (block-split)
                command: self.current_command.clone(),
                cwd: None,
                started_at: self
                    .command_started_at
                    .unwrap_or_else(std::time::SystemTime::now),
            }),
            command_finished: self.command_finished.take(),
            history_cleared: std::mem::take(&mut self.history_cleared_edge),
        }
    }

    /// Region-tagged bookkeeping done just before a segment is forwarded.
    fn pre_forward(&mut self, seg: &[u8]) {
        if self.region == PromptRegion::Command {
            self.command_buf.extend_from_slice(seg);
        }
    }

    fn apply(&mut self, sub: u8, next: Option<PromptRegion>, exit: Option<i32>) {
        let Some(r) = next else {
            if sub == b'K' {
                self.history_cleared_edge = true;
            }
            return;
        };

        // Whether trust was established BEFORE this mark: the trust-granting `;D` itself
        // (which closes the session-start synthetic `;A;B;C` prime, or any later
        // trust-recovery cycle) must not produce a command block (command-blocks).
        let was_trusted = self.boundary_trusted();

        if !matches!(
            (self.region, r),
            (PromptRegion::None, PromptRegion::None)
                | (PromptRegion::None, PromptRegion::Prompt)
                | (PromptRegion::Prompt, PromptRegion::Prompt)
                | (PromptRegion::Prompt, PromptRegion::Command)
                | (PromptRegion::Command, PromptRegion::Output)
                | (PromptRegion::Output, PromptRegion::None)
                | (PromptRegion::Output, PromptRegion::Prompt)
        ) || !self.advance_lifecycle(r)
        {
            self.reset_boundary_state();
            return;
        }

        // Prompt started (;A, including a re-rendered prompt).
        if r == PromptRegion::Prompt {
            self.prompt_start_edge = true;
        }
        // Command output started (;C).
        if r == PromptRegion::Output && self.region != PromptRegion::Output {
            self.command_started_at = Some(std::time::SystemTime::now());
            // Render the echo once here; the ;D capture reuses it. A trusted,
            // non-empty start becomes the in-flight block.
            self.current_command = render_command_echo(&self.command_buf);
            self.command_started_edge = was_trusted && !self.current_command.is_empty();
        }
        if r == PromptRegion::Command {
            self.command_buf.clear();
        }
        // Command finished (;D) on an ordered lifecycle: finalize a command block — only
        // when trust was already established before this mark (skips the synthetic-prime
        // and trust-recovery cycles) and the command text is non-empty (an empty Enter
        // carries no useful metadata; Warp builds no block for it either).
        if sub == b'D' && r == PromptRegion::None {
            let started_at = self.command_started_at.take();
            let command = std::mem::take(&mut self.current_command);
            self.command_buf.clear();
            if was_trusted && !command.is_empty() {
                if let Some(started_at) = started_at {
                    self.command_finished = Some(CommandCapture {
                        seq: 0, // stamped by the mark-closure caller (block-split)
                        command,
                        exit_code: exit,
                        cwd: None, // filled by the caller from its ;C latch
                        started_at,
                        ended_at: std::time::SystemTime::now(),
                    });
                }
            }
        }
        if r != self.region && prompt_trace_enabled() {
            eprintln!("[prompt133] {:?} -> {:?}", self.region, r);
        }
        self.region = r;
    }

    fn advance_lifecycle(&mut self, r: PromptRegion) -> bool {
        match (self.lifecycle, r) {
            (ShellLifecycleProgress::AwaitPrompt, PromptRegion::Prompt)
            | (ShellLifecycleProgress::InPrompt, PromptRegion::Prompt) => {
                self.lifecycle = ShellLifecycleProgress::InPrompt;
                true
            }
            (ShellLifecycleProgress::InPrompt, PromptRegion::Command) => {
                self.lifecycle = ShellLifecycleProgress::InCommand;
                true
            }
            (ShellLifecycleProgress::InCommand, PromptRegion::Output) => {
                self.lifecycle = ShellLifecycleProgress::InOutput;
                true
            }
            (ShellLifecycleProgress::InOutput, PromptRegion::None) => {
                self.lifecycle = ShellLifecycleProgress::AwaitPrompt;
                self.set_boundary_trust(true);
                true
            }
            _ => false,
        }
    }

    fn reset_boundary_state(&mut self) {
        self.set_boundary_trust(false);
        self.lifecycle = ShellLifecycleProgress::AwaitPrompt;
        self.region = PromptRegion::None;
        self.command_buf.clear();
        self.command_started_at = None;
        self.current_command.clear();
        self.prompt_start_edge = false;
        self.command_started_edge = false;
        // `command_finished` is kept: it was produced by a lifecycle that completed
        // trusted before this glitch, so the block is still valid history (it drains
        // at the very next mark's hook).
    }

    fn set_boundary_trust(&mut self, trusted: bool) {
        let next = if trusted {
            ShellBoundaryTrust::Trusted
        } else {
            ShellBoundaryTrust::Untrusted
        };
        if self.boundary_trust != next {
            self.boundary_trust = next;
            self.boundary_trust_changed = Some(trusted);
        }
    }

    fn boundary_trusted(&self) -> bool {
        self.boundary_trust == ShellBoundaryTrust::Trusted
    }

    fn take_boundary_trust_changed(&mut self) -> Option<bool> {
        self.boundary_trust_changed.take()
    }
}

/// Gated on `NMT_PROMPT_TRACE` so live OSC 133 region transitions can be logged during
/// classify-only validation; zero-cost when unset (one `OnceLock` load).
fn prompt_trace_enabled() -> bool {
    static EN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *EN.get_or_init(|| std::env::var_os("NMT_PROMPT_TRACE").is_some())
}

pub struct PtyPipe<T: nmt_platform::EventedPty, U: EventListener> {
    sender: MsgSender,
    receiver: mpsc::Receiver<Msg>,
    pty: T,
    poll: Poll,
    /// The loop's `Waker`. On Windows the ConPTY worker threads and the child-exit
    /// callback signal readiness through it; the `MsgSender` wakes it after each send
    /// (mio 1.2 has no pollable channel / user-space readiness).
    waker: Arc<Waker>,
    /// Ghostty VT engine driving the terminal state. Shared behind a lock so the
    /// frontend can reach it synchronously for scroll, selection, and search. The
    /// PTY thread locks it for `write_vt` and render-state reads; the
    /// render buffer is on a separate lock so a frame never waits behind a parse.
    ghostty: Arc<FairMutex<GhosttyTerminal>>,
    /// Ghostty-fed viewport copy the renderer reads. Populated here
    /// each batch; on its own lock, separate from the engine so paint never
    /// waits behind parsing.
    render_buffer: Arc<FairMutex<RenderBuffer>>,
    /// PTY-thread-private target for direct Ghostty capture. A completed frame
    /// swaps with `render_buffer`, so the shared lock covers only publication.
    back_buffer: RenderBuffer,
    /// VT modes published to the frontend. This `PtyPipe` is the sole writer;
    /// the input path reads it lock-free. `Mode` is `u32`.
    vt_modes: Arc<AtomicU32>,
    /// Monotonic content version, bumped once per PTY batch / resize (the engine
    /// exposes no generation signal). The frontend's deep-search corpus cache
    /// compares it to detect content changes and invalidate cached views.
    content_version: Arc<AtomicU64>,
    /// Optional observer for the exact VT stream accepted by the engine. It runs
    /// under the engine lock so a checkpoint and its following byte sequence can
    /// be ordered atomically; observers must therefore return promptly.
    output_sink: Option<Arc<dyn Fn(Arc<[u8]>) + Send + Sync>>,
    terminal_responses_enabled: bool,
    event_proxy: U,
    window_id: WindowId,
    route_id: usize,
    conpty_resize_echo_realign: bool,
    conpty_resize_echo_pending: bool,
    conpty_resize_repaint_reads_remaining: u8,
    /// When the last resize happened. The `conpty_resize_echo_realign` input
    /// gate only fires for a brief window after a resize — input typed *during*
    /// the resize repaint, whose ConPTY echo lands at a stale CUP. Without a time
    /// bound, ordinary typing long after a resize keeps tripping the gate and
    /// accumulates (the "type 120 x's after a resize and the prompt repeats" bug).
    conpty_resize_at: Option<std::time::Instant>,
    /// SU-realign latch. `active_cursor_row()` is captured at resize time
    /// (it flips within one frame during the ConPTY repaint storm, so it can't be read
    /// live). `*_cols/_rows` let the first repaint sanity-check it answers this resize.
    conpty_resize_prompt_row: u16,
    conpty_resize_cols: u16,
    conpty_resize_rows: u16,
    su_realign_armed: bool,
    /// OSC 133 region state; only touched on the PTY thread.
    sniffer: PromptSniffer,
    /// Launch cwd of the currently-running command, latched at its `;C`: the cwd MUST
    /// NOT be read at `;D`, by which point the ps1 has already reported the NEXT
    /// prompt's OSC 7 directory (which would mislabel every `cd`). Attached to the
    /// CommandFinished event. PTY-thread-private.
    launch_cwd: Option<Option<std::path::PathBuf>>,
    /// Engine-blocks mode is the default: at
    /// each trusted `;D` the engine freezes the command into a finished
    /// block (`finish_block`, O(1)) whose handle is shipped to the app's
    /// block store; rendering reads the frozen block directly. `false` is
    /// the classic single-grid fallback: no finish, no boundary
    /// clear, no block events — plain terminal behavior.
    engine_blocks: bool,
    /// OSC 133 prompt-boundary sequence number; incremented at each trusted
    /// `;A` and stamped onto Command* events for segment/metadata marriage.
    mark_seq: u64,
    /// Last-seen alt-screen state, for edge-triggered interactive-state events.
    prev_alt_screen: bool,
    /// Last InteractiveState value sent, so the signal stays edge-triggered.
    prev_interactive: bool,
    /// Last AltScreen value sent.
    prev_alt_screen_sent: bool,
    /// When the last `snapshot()` readback ran, for saturation coalescing
    /// (`SNAPSHOT_MIN_INTERVAL`). PTY-thread-private.
    last_snapshot_at: std::time::Instant,
    /// True when a saturated batch or synchronized update deferred its readback.
    /// Makes the event loop run `pty_read` without requiring new PTY bytes.
    snapshot_pending: bool,
    /// Start of the current DEC 2026 transaction. The event-loop poll uses this
    /// deadline to recover when an application omits the matching reset.
    sync_output_started_at: Option<std::time::Instant>,
}

/// Read the VT-controlled modes from the Ghostty engine into `Mode`.
/// bits (e.g. `Mode::VI`) are not touched here — see
/// [`Crosswords::sync_vt_modes`].
fn ghostty_vt_modes(g: &GhosttyTerminal) -> crate::terminal::Mode {
    use crate::ghostty::mode as gm;
    use crate::terminal::Mode;
    let mut m = Mode::empty();
    m.set(Mode::SHOW_CURSOR, g.mode(gm::CURSOR_VISIBLE));
    m.set(Mode::APP_CURSOR, g.mode(gm::CURSOR_KEYS));
    m.set(Mode::APP_KEYPAD, g.mode(gm::KEYPAD_KEYS));
    m.set(Mode::MOUSE_REPORT_CLICK, g.mode(gm::MOUSE_NORMAL));
    m.set(Mode::MOUSE_DRAG, g.mode(gm::MOUSE_BUTTON));
    m.set(Mode::MOUSE_MOTION, g.mode(gm::MOUSE_ANY));
    m.set(Mode::SGR_MOUSE, g.mode(gm::MOUSE_SGR));
    m.set(Mode::UTF8_MOUSE, g.mode(gm::MOUSE_UTF8));
    m.set(Mode::ALTERNATE_SCROLL, g.mode(gm::MOUSE_ALTERNATE_SCROLL));
    m.set(Mode::BRACKETED_PASTE, g.mode(gm::BRACKETED_PASTE));
    m.set(Mode::FOCUS_IN_OUT, g.mode(gm::FOCUS_EVENT));
    m.set(Mode::LINE_WRAP, g.mode(gm::WRAPAROUND));
    m.set(Mode::INSERT, g.mode(gm::INSERT));
    m.set(Mode::ALT_SCREEN, g.mode(gm::ALT_SCREEN));
    // Kitty keyboard protocol flags live in a separate engine stack, not the DEC
    // modes, so fold them in here to enable Kitty press and key-release encoding.
    m |= g.kitty_keyboard_modes();
    m
}

fn publish_render_buffer(
    front: &FairMutex<RenderBuffer>,
    back: &mut RenderBuffer,
    capture: crate::ghostty::Result<()>,
) -> bool {
    if capture.is_err() {
        return false;
    }
    std::mem::swap(&mut *front.lock(), back);
    true
}

/// Convert an OSC 7 working-directory value to a path. Strips a `file://host`
/// prefix when present (`file://host/path` → `/path`); otherwise uses the
/// value verbatim.
/// Convert `scrollback-history-limit` (in **lines**) to the engine's
/// scrollback byte budget. The engine stores rows in byte-bounded pages
/// (~14 B/cell observed), so we size for `cols × 16 B/cell` — 16 is an upper bound,
/// guaranteeing *at least* `lines` rows at the creation width. Approximate by
/// nature: a later resize-widen can hold fewer than `lines` rows, and the engine
/// floors very small budgets at ~one page. `lines == 0` → 0 (engine minimum).
fn scrollback_bytes(lines: usize, cols: u16) -> usize {
    const BYTES_PER_CELL: usize = 16;
    lines
        .saturating_mul(cols as usize)
        .saturating_mul(BYTES_PER_CELL)
}

pub(crate) fn pwd_to_path(pwd: &str) -> std::path::PathBuf {
    if let Some(rest) = pwd.strip_prefix("file://") {
        // rest = "host/path"; the path starts at the first '/'.
        if let Some(slash) = rest.find('/') {
            return std::path::PathBuf::from(&rest[slash..]);
        }
    }
    std::path::PathBuf::from(pwd)
}

#[derive(Default)]
pub struct PtyState {
    write_list: VecDeque<Cow<'static, [u8]>>,
    writing: Option<Writing>,
}

impl PtyState {
    #[inline]
    fn ensure_next(&mut self) {
        if self.writing.is_none() {
            self.goto_next();
        }
    }

    #[inline]
    fn goto_next(&mut self) {
        self.writing = self.write_list.pop_front().map(Writing::new);
    }

    #[inline]
    fn take_current(&mut self) -> Option<Writing> {
        self.writing.take()
    }

    #[inline]
    fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }

    #[inline]
    fn set_current(&mut self, new: Option<Writing>) {
        self.writing = new;
    }
}

struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

impl Writing {
    #[inline]
    fn new(c: Cow<'static, [u8]>) -> Writing {
        Writing {
            source: c,
            written: 0,
        }
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.written += n;
    }

    #[inline]
    fn remaining_bytes(&self) -> &[u8] {
        &self.source[self.written..]
    }

    #[inline]
    fn finished(&self) -> bool {
        self.written >= self.source.len()
    }
}

impl<T, U> PtyPipe<T, U>
where
    T: nmt_platform::EventedPty + Send + 'static,
    U: EventListener + Send + 'static,
{
    pub fn new(
        render_buffer: Arc<FairMutex<RenderBuffer>>,
        vt_modes: Arc<AtomicU32>,
        pty: T,
        event_proxy: U,
        window_id: WindowId,
        route_id: usize,
        colors: nmt_config::colors::Colors,
        scrollback_lines: usize,
        engine_blocks: bool,
    ) -> Result<PtyPipe<T, U>, Box<dyn std::error::Error>> {
        let poll = Poll::new()?;
        // The `Waker` is registered on a reserved token; the worker threads (Windows)
        // and the `MsgSender` wake the loop through it.
        let waker = Arc::new(Waker::new(poll.registry(), WAKER_TOKEN)?);
        let (tx, rx) = mpsc::channel();
        let sender = MsgSender::new(tx, waker.clone());

        // Start the engine at the render buffer's viewport dimensions so the first
        // resize cannot diverge from a zero-sized construction.
        let (cols, rows) = {
            let rb = render_buffer.lock();
            (rb.cols() as u16, rb.rows() as u16)
        };
        // `max_scrollback` is a **byte budget** in the engine — the C-binding's
        // "lines" doc is wrong (verified: budgets ≤1 MB floor at ~3297 lines, 10 MB
        // holds ~36k at 20 cols ≈ 273 B/line). `scrollback-history-limit` is
        // in lines, so convert through `scrollback_bytes`.
        let max_scrollback = scrollback_bytes(scrollback_lines, cols.max(1));
        let mut ghostty = GhosttyTerminal::new(cols.max(1), rows.max(1), max_scrollback)
            .map_err(|err| Box::new(err) as Box<dyn std::error::Error>)?;

        // Push the host theme's default colors + 256-palette into the engine so
        // SGR-indexed and default colors resolve to theme.
        ghostty.set_theme_colors(&colors);

        // Seed the atomic with the engine's initial VT modes (SHOW_CURSOR,
        // LINE_WRAP, …) so the facade is correct before the first PTY batch.
        vt_modes.store(
            ghostty_vt_modes(&ghostty).bits(),
            std::sync::atomic::Ordering::Relaxed,
        );

        Ok(PtyPipe {
            sender,
            receiver: rx,
            poll,
            waker,
            pty,
            ghostty: Arc::new(FairMutex::new(ghostty)),
            render_buffer,
            back_buffer: RenderBuffer::new(cols as usize, rows as usize),
            vt_modes,
            content_version: Arc::new(AtomicU64::new(0)),
            output_sink: None,
            terminal_responses_enabled: true,
            event_proxy,
            window_id,
            route_id,
            conpty_resize_echo_realign: false,
            conpty_resize_echo_pending: false,
            conpty_resize_repaint_reads_remaining: 0,
            conpty_resize_at: None,
            conpty_resize_prompt_row: 0,
            conpty_resize_cols: 0,
            conpty_resize_rows: 0,
            su_realign_armed: false,
            sniffer: PromptSniffer::default(),
            launch_cwd: None,
            engine_blocks,
            mark_seq: 0,
            prev_alt_screen: false,
            prev_interactive: false,
            prev_alt_screen_sent: false,
            last_snapshot_at: std::time::Instant::now(),
            snapshot_pending: false,
            sync_output_started_at: None,
        })
    }

    /// Emit alt-screen interactive state, edge-triggered so a stable state emits nothing.
    fn emit_interactive_state(&mut self) {
        let on = self.prev_alt_screen;
        if on != self.prev_interactive {
            self.prev_interactive = on;
            self.event_proxy
                .send_event(TerminalEvent::InteractiveState(on), self.window_id);
        }
        if self.prev_alt_screen != self.prev_alt_screen_sent {
            self.prev_alt_screen_sent = self.prev_alt_screen;
            self.event_proxy.send_event(
                TerminalEvent::AltScreen(self.prev_alt_screen),
                self.window_id,
            );
        }
    }

    /// Shared engine handle for frontend scrolling, selection, search, and rendering.
    /// The PTY thread and frontend
    /// serialize access through its lock.
    pub fn engine(&self) -> Arc<FairMutex<GhosttyTerminal>> {
        Arc::clone(&self.ghostty)
    }

    /// Shared content version for invalidating the frontend's deep-search corpus cache.
    /// Bumped per PTY batch or resize; the frontend reads it lock-free.
    pub fn content_version(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.content_version)
    }

    /// Observe parsed VT output without taking ownership of the PTY loop.
    pub fn set_output_sink(&mut self, sink: impl Fn(Arc<[u8]>) + Send + Sync + 'static) {
        self.output_sink = Some(Arc::new(sink));
    }

    /// Choose whether VT-generated DA/DSR/OSC replies are written to this PTY.
    pub fn set_terminal_responses_enabled(&mut self, enabled: bool) {
        self.terminal_responses_enabled = enabled;
    }

    #[inline]
    fn pty_read(&mut self, _state: &mut PtyState, buf: &mut [u8]) -> io::Result<()> {
        let mut unprocessed = 0;
        let mut processed = 0;
        // True when the loop drained the PTY (WouldBlock/EOF); false when it
        // broke at MAX_LOCKED_READ with more data likely pending.
        let mut caught_up = false;

        loop {
            // Read from the PTY.
            match self.pty.reader().read(&mut buf[unprocessed..]) {
                // This is received on Windows/macOS when no more data is readable from the PTY.
                Ok(0) if unprocessed == 0 => {
                    caught_up = true;
                    break;
                }
                Ok(got) => unprocessed += got,
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                        // Go back to mio if we're caught up on parsing and the PTY would block.
                        if unprocessed == 0 {
                            caught_up = true;
                            break;
                        }
                    }
                    _ => return Err(err),
                },
            }

            // Feed the bytes into Ghostty's VT engine (under the engine lock).
            // The render buffer is on a separate lock, so this never blocks a
            // render frame while the same engine snapshot is locked.
            let output_sink = self.output_sink.clone();
            {
                let mut engine = self.ghostty.lock();
                let echo_pending_at_entry = cfg!(windows) && self.conpty_resize_echo_pending;
                let mut rewritten = None;
                let mut synthetic_prefix = None;
                // Some(R_conpty) means SU realignment ran during this read.
                let mut su_realigned_to: Option<u16> = None;
                let repaint_window =
                    cfg!(windows) && self.conpty_resize_repaint_reads_remaining > 0;
                if repaint_window {
                    self.conpty_resize_repaint_reads_remaining =
                        self.conpty_resize_repaint_reads_remaining.saturating_sub(1);
                }
                // SU realignment runs before and instead of the legacy CUP rewrite.
                // On the first repaint of a resize, push ghostty's active-top history rows
                // into scrollback so its prompt rises to ConPTY's row; then ConPTY's own
                // CUP in the ORIGINAL repaint lands on the prompt (no 0005 rewrite). The
                // `su_realign_count` pre-check (latched size correspondence + N>0) and the
                // post-write assertion below guard against a mis-latched resize.
                if cfg!(windows) && self.su_realign_armed && repaint_window {
                    self.su_realign_armed = false;
                    if !engine.mode(mode::ALT_SCREEN) {
                        if let Some((n, r_conpty)) = su_realign_count(
                            &buf[..unprocessed],
                            self.conpty_resize_prompt_row,
                            self.conpty_resize_cols,
                            self.conpty_resize_rows,
                            engine.cols(),
                            engine.rows(),
                        ) {
                            let realign = format!("\x1b[{n}S").into_bytes();
                            engine.write_vt(&realign);
                            synthetic_prefix = Some(realign);
                            su_realigned_to = Some(r_conpty);
                            self.conpty_resize_echo_pending = false;
                        }
                    }
                }
                if cfg!(windows)
                    && su_realigned_to.is_none()
                    && (self.conpty_resize_echo_pending || repaint_window)
                {
                    if engine.mode(mode::ALT_SCREEN) {
                        self.conpty_resize_echo_pending = false;
                        self.conpty_resize_repaint_reads_remaining = 0;
                    } else if let Some(active_row) = engine.active_cursor_row() {
                        // Realign ConPTY's resize echo/repaint to the engine's ACTIVE
                        // cursor row. `active_cursor_row()` reads the active-screen
                        // cursor (the frame CUP addresses) straight from the engine, so
                        // it stays correct regardless of viewport scroll or blank rows
                        // below the prompt — unlike the render-state
                        // `snapshot.cursor.y`, which is viewport-relative and reads as
                        // row 0 when scrolled (forcing the echo+erase onto a visible
                        // history row). With a reliable target there's no need for the
                        // old at-bottom gate or the PTY-thread snap-to-bottom; the echo
                        // always routes to the true prompt row (the frontend still
                        // snaps the *view* to the bottom on keypress for UX).
                        let target_row = active_row.saturating_add(1);
                        let repaint_pending = repaint_window
                            && is_conpty_resize_repaint(&buf[..unprocessed], target_row);
                        if self.conpty_resize_echo_pending || repaint_pending {
                            rewritten = rewrite_conpty_resize_echo_cup_rows(
                                &buf[..unprocessed],
                                target_row,
                            );
                        }
                        if rewritten.is_some() || repaint_pending {
                            self.conpty_resize_echo_pending = false;
                            if repaint_pending {
                                self.conpty_resize_repaint_reads_remaining = 0;
                            }
                        }
                    }
                }
                let bytes = rewritten.as_deref().unwrap_or(&buf[..unprocessed]);
                let observed_output = output_sink.as_ref().map(|_| match synthetic_prefix {
                    Some(mut prefix) => {
                        prefix.extend_from_slice(bytes);
                        Arc::from(prefix)
                    }
                    None => Arc::from(bytes),
                });
                let was_rewritten = rewritten.is_some();
                // Capture the pre-rewrite ConPTY bytes + cursor visibility for the
                // trace so we can compare original vs rewritten CUP rows.
                let (orig_escaped, cursor_vis_at_decision) = if crate::vt_trace::enabled()
                    && (repaint_window || was_rewritten || echo_pending_at_entry)
                {
                    let orig = &buf[..unprocessed];
                    (
                        escape_bytes(&orig[..orig.len().min(240)]),
                        engine.snapshot().ok().map(|s| s.cursor_visible()),
                    )
                } else {
                    (String::new(), None)
                };
                // Always run the sniffer: it classifies/captures OSC 133 lifecycle state
                // while every byte, including marks, still reaches the engine.
                {
                    {
                        // Both feed closures need the engine (segment/mark forwarding
                        // writes; cwd latch reads) and never run at the same time, so a
                        // RefCell shares the one borrow.
                        let engine_cell = std::cell::RefCell::new(&mut *engine);
                        let launch_cwd = &mut self.launch_cwd;
                        let mark_seq = &mut self.mark_seq;
                        let event_proxy = &self.event_proxy;
                        let window_id = self.window_id;
                        let engine_blocks = self.engine_blocks;
                        self.sniffer.feed_hooked(
                            bytes,
                            |_, _, seg| {
                                engine_cell.borrow_mut().write_vt(seg);
                            },
                            |mut mark| {
                                engine_cell.borrow_mut().write_vt(mark.bytes);
                                if mark.prompt_started && mark.trusted {
                                    // The block IS the segment: only the sequence
                                    // number is registered here, for metadata
                                    // marriage at the eventual finish.
                                    *mark_seq += 1;
                                    event_proxy.send_event(TerminalEvent::PromptStarted, window_id);
                                }
                                if let Some(mut start) = mark.command_started.take() {
                                    // ;C — latch the launch cwd while the engine still
                                    // holds THIS prompt's OSC 7, and surface the in-flight block.
                                    let cwd = engine_cell.borrow().current_directory();
                                    start.seq = *mark_seq;
                                    start.cwd = cwd.clone();
                                    *launch_cwd = Some(cwd);
                                    event_proxy.send_event(
                                        TerminalEvent::CommandStarted(start),
                                        window_id,
                                    );
                                }
                                if let Some(mut cmd) = mark.command_finished.take() {
                                    // ;D — attach the launch metadata.
                                    let cwd = launch_cwd.take().unwrap_or(None);
                                    cmd.seq = *mark_seq;
                                    cmd.cwd = cwd;
                                    let in_alt_screen = engine_cell.borrow().mode(mode::ALT_SCREEN);
                                    if mark.trusted && !in_alt_screen && engine_blocks {
                                        // Engine-blocks mode freezes
                                        // the command into a finished engine block
                                        // (O(1)) and hands the HANDLE to the store —
                                        // rendering reads the block via `BlockRef`;
                                        // nothing is materialized. Budget eviction may
                                        // fire inside finish_block, so the same batch
                                        // carries the live list for the store to prune
                                        // against. Clearing the boundary gives ConPTY's
                                        // cursor model a fresh grid for the next command.
                                        let mut engine = engine_cell.borrow_mut();
                                        let events = match engine.finish_block() {
                                            Ok(Some(handle)) => {
                                                let rows =
                                                    engine.block_row_count(handle).unwrap_or(0);
                                                vec![
                                                    crate::event::BlockEvent::EngineBlock {
                                                        seq: *mark_seq,
                                                        handle,
                                                        rows,
                                                    },
                                                    crate::event::BlockEvent::EngineBlocksSync(
                                                        engine_blocks_live_list(&engine),
                                                    ),
                                                ]
                                            }
                                            // Empty command: no block, no segment.
                                            Ok(None) => Vec::new(),
                                            Err(err) => {
                                                tracing::warn!("finish_block failed: {err:?}");
                                                Vec::new()
                                            }
                                        };
                                        if !events.is_empty() {
                                            event_proxy.send_event(
                                                TerminalEvent::BlockBatch(events),
                                                window_id,
                                            );
                                        }
                                        engine.write_vt(BLOCK_BOUNDARY_CLEAR);
                                    }
                                    // Classic mode keeps one continuous grid:
                                    // no finish, no boundary clear — plain single
                                    // grid; only the metadata event fires.
                                    event_proxy
                                        .send_event(TerminalEvent::CommandFinished(cmd), window_id);
                                }
                                if mark.history_cleared && mark.trusted && engine_blocks {
                                    // ;K — the Clear-Host wrapper announces a user
                                    // A trusted clear drops every finished engine
                                    // block and wipe the active grid (the shell's
                                    // own clear follows through ConPTY).
                                    let in_alt_screen = engine_cell.borrow().mode(mode::ALT_SCREEN);
                                    if !in_alt_screen {
                                        engine_cell.borrow_mut().write_vt(BLOCK_BOUNDARY_CLEAR);
                                        engine_cell.borrow_mut().clear_blocks();
                                        event_proxy.send_event(
                                            TerminalEvent::BlockBatch(vec![
                                                crate::event::BlockEvent::HistoryCleared,
                                            ]),
                                            window_id,
                                        );
                                    }
                                }
                            },
                        );
                    }
                    if let Some(trusted) = self.sniffer.take_boundary_trust_changed() {
                        self.event_proxy.send_event(
                            TerminalEvent::PromptBoundaryTrusted(trusted),
                            self.window_id,
                        );
                    }
                    // No steady-state work per read: the whole command is
                    // frozen once at its `;D` finish (the per-line harvest
                    // this replaced was the throughput cost the per-block
                    // grid exists to delete).
                }
                // After SU realignment and the original repaint, ConPTY's own
                // absolute CUP must have pulled the active cursor onto the realigned
                // prompt row. A mismatch means the realign assumption broke (mis-latched
                // resize, divergent wrap) — trace + debug_assert, never a prod panic.
                if let Some(expected) = su_realigned_to {
                    let got = engine.active_cursor_row();
                    if got != Some(expected) && crate::vt_trace::enabled() {
                        crate::vt_trace::trace(
                            "su_realign_assert",
                            &mut engine,
                            &format!("expected R_conpty={expected} got={got:?}"),
                        );
                    }
                    debug_assert_eq!(
                        got,
                        Some(expected),
                        "SU realign: active cursor should land on R_conpty"
                    );
                }
                // Dump the full VT only for resize-related reads (the ConPTY repaint
                // window or a realigned echo) — a per-keystroke full dump would be
                // O(scrollback) on every read.
                if crate::vt_trace::enabled()
                    && (repaint_window || was_rewritten || echo_pending_at_entry)
                {
                    crate::vt_trace::trace(
                        "pty_resize_read",
                        &mut engine,
                        &format!(
                            "read_bytes={unprocessed} rewritten={was_rewritten} \
                             repaint_window={repaint_window} echo_pending={echo_pending_at_entry} \
                             cursor_vis_pre={cursor_vis_at_decision:?} \
                             orig={orig_escaped} \
                             bytes={}",
                            escape_bytes(&bytes[..bytes.len().min(240)])
                        ),
                    );
                }
                if let (Some(sink), Some(output)) = (&output_sink, observed_output) {
                    sink(output);
                }
            }
            // Content changed, so invalidate the cached deep-search corpus.
            self.content_version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            processed += unprocessed;
            unprocessed = 0;

            // Don't accumulate unboundedly before reflecting to the renderer.
            if processed >= MAX_LOCKED_READ {
                break;
            }
        }

        if processed == 0 && !self.snapshot_pending {
            return Ok(());
        }

        // Coalesce the full-viewport readback: a caught-up read always
        // snapshots (interactive echo and the stream's final state land with no
        // added latency); a saturated read rate-limits it to
        // SNAPSHOT_MIN_INTERVAL so the readback stops dominating parse
        // throughput (see the constant's doc).
        let do_snapshot = caught_up || self.last_snapshot_at.elapsed() >= SNAPSHOT_MIN_INTERVAL;

        // Drain everything from the engine under ONE lock: query/DSR/DA
        // responses, bell, title, pwd, VT modes, and the snapshot. Then act on
        // the owned results below with the engine lock released.
        let (responses, bell, clipboard_writes, title, vt_modes, sync_output, capture, image_delta) = {
            let mut engine = self.ghostty.lock();
            let responses = engine.take_pty_writes();
            let bell = engine.take_bell();
            let clipboard_writes = engine.take_clipboard_writes();
            let title = engine.poll_title();
            let vt_modes = ghostty_vt_modes(&engine);
            let sync_output_timed_out = self
                .sync_output_started_at
                .is_some_and(|started| started.elapsed() >= SYNC_OUTPUT_TIMEOUT);
            if sync_output_timed_out && engine.mode(mode::SYNC_OUTPUT) {
                engine.write_vt(b"\x1b[?2026l");
            }
            let sync_output = engine.mode(mode::SYNC_OUTPUT);
            let (capture, image_delta) = if do_snapshot && !sync_output {
                let capture = engine.snapshot_into(&mut self.back_buffer);
                // Kitty image pixel deltas, under the same lock. Only the PTY
                // reader path drives image shipping; the scroll path never calls this.
                let image_delta = match &capture {
                    Ok(()) => engine.take_image_deltas(self.back_buffer.placements()),
                    Err(_) => (Vec::new(), Vec::new()),
                };
                (Some(capture), image_delta)
            } else {
                (None, (Vec::new(), Vec::new()))
            };
            (
                responses,
                bell,
                clipboard_writes,
                title,
                vt_modes,
                sync_output,
                capture,
                image_delta,
            )
        };

        // Ship new/changed kitty image pixels + removals via the existing graphics
        // event → `sugarloaf.image_data`. Empty in steady state.
        let (pending_images, removed_ids) = image_delta;
        if !pending_images.is_empty() || !removed_ids.is_empty() {
            use crate::graphics::GraphicId;
            self.event_proxy.send_event(
                TerminalEvent::UpdateGraphics {
                    route_id: self.route_id,
                    queues: crate::ansi::graphics::UpdateQueues {
                        pending: Vec::new(),
                        pending_images,
                        remove_queue: removed_ids
                            .into_iter()
                            .map(|id| GraphicId(id as u64))
                            .collect(),
                    },
                },
                self.window_id,
            );
        }

        if self.terminal_responses_enabled && !responses.is_empty() {
            let _ = self.pty.writer().write_all(&responses);
        }
        if bell > 0 {
            self.event_proxy
                .send_event(TerminalEvent::Bell, self.window_id);
        }
        for (ty, text) in clipboard_writes {
            self.event_proxy
                .send_event(TerminalEvent::ClipboardStore(ty, text), self.window_id);
        }
        if let Some(title) = title {
            self.event_proxy
                .send_event(TerminalEvent::Title(title), self.window_id);
        }
        // Publish VT modes lock-free; this PTY thread is the sole writer.
        self.vt_modes
            .store(vt_modes.bits(), std::sync::atomic::Ordering::Relaxed);

        if sync_output {
            self.sync_output_started_at
                .get_or_insert_with(std::time::Instant::now);
        } else {
            self.sync_output_started_at = None;
        }

        // Interactive-state detection: full-screen TUIs set alt-screen.
        self.prev_alt_screen = vt_modes.contains(crate::terminal::Mode::ALT_SCREEN);
        self.emit_interactive_state();

        // Readback skipped under saturation: self-wake so another `pty_read`
        // pass runs even if the pipe drained exactly at the MAX_LOCKED_READ
        // boundary (no OS readiness would re-fire, and the Windows soft-ready
        // flag may already be clear). That pass either parses more pending data
        // or reads 0 bytes, lands caught-up, and flushes this pending snapshot.
        let Some(capture) = capture else {
            self.snapshot_pending = true;
            // A synchronized frame needs more PTY bytes (normally DEC reset 2026)
            // before it is safe to publish. Waking immediately would spin while the
            // pipe is empty; the next readable event will commit the complete frame.
            if !sync_output {
                let _ = self.waker.wake();
            }
            return Ok(());
        };
        self.last_snapshot_at = std::time::Instant::now();
        self.snapshot_pending = false;

        if publish_render_buffer(&self.render_buffer, &mut self.back_buffer, capture) {
            self.event_proxy.send_event(
                TerminalEvent::TerminalDamaged(self.route_id),
                self.window_id,
            );
        }

        Ok(())
    }

    /// Drain the channel.
    ///
    /// Returns `false` when a shutdown message was received.
    fn drain_recv_channel(&mut self, state: &mut PtyState) -> bool {
        while let Ok(msg) = self.receiver.try_recv() {
            match msg {
                Msg::Input(input) => {
                    // Only treat input as a resize echo for a brief window after a
                    // resize (the keystroke typed *during* the repaint, whose ConPTY
                    // echo lands at a stale CUP). The `reads_remaining` countdown
                    // doesn't close this window when the user is idle then types a
                    // fast burst (one channel drain → one `pty_read`), so bound it by
                    // time. Without this, ordinary typing after a resize keeps being
                    // realigned and the input/prompt accumulates.
                    const RESIZE_ECHO_WINDOW: std::time::Duration =
                        std::time::Duration::from_millis(150);
                    if cfg!(windows)
                        && self.conpty_resize_echo_realign
                        && self
                            .conpty_resize_at
                            .is_some_and(|t| t.elapsed() < RESIZE_ECHO_WINDOW)
                        && is_conpty_resize_echo_input(input.as_ref())
                    {
                        self.conpty_resize_echo_pending = true;
                    }
                    state.write_list.push_back(input)
                }
                Msg::Resize(window_size) => {
                    // Keep the Ghostty engine sized to match the PTY/Crosswords.
                    let cols = window_size.cols.max(1);
                    let rows = window_size.rows.max(1);
                    let cell_w = (window_size.width / cols).max(1) as u32;
                    let cell_h = (window_size.height / rows).max(1) as u32;
                    let mut blocks_sync: Option<Vec<(crate::ghostty::BlockHandle, usize)>> = None;
                    let (snapshot, active_row) = {
                        let mut engine = self.ghostty.lock();
                        if crate::vt_trace::enabled() {
                            crate::vt_trace::trace(
                                "perf_resize_before",
                                &mut engine,
                                &format!(
                                    "request cols={} rows={} px={}x{} cell={}x{}",
                                    cols,
                                    rows,
                                    window_size.width,
                                    window_size.height,
                                    cell_w,
                                    cell_h
                                ),
                            );
                        }
                        if let Err(err) = engine.resize(cols, rows, cell_w, cell_h) {
                            tracing::warn!("engine resize failed: {err:?}");
                        }
                        if crate::vt_trace::enabled() {
                            crate::vt_trace::trace(
                                "perf_resize_after_engine",
                                &mut engine,
                                &format!(
                                    "applied cols={} rows={} cell={}x{}",
                                    cols, rows, cell_w, cell_h
                                ),
                            );
                        }
                        // Engine-blocks: resize eagerly reflowed every finished
                        // block (new generations + row counts) — ship the fresh
                        // list so the store's cached layout follows the engine reflow.
                        if self.engine_blocks && engine.block_count() > 0 {
                            blocks_sync = Some(engine_blocks_live_list(&engine));
                        }
                        let capture = engine.snapshot_into(&mut self.back_buffer);
                        (capture, engine.active_cursor_row())
                    };
                    if let Some(live) = blocks_sync {
                        self.event_proxy.send_event(
                            TerminalEvent::BlockBatch(vec![
                                crate::event::BlockEvent::EngineBlocksSync(live),
                            ]),
                            self.window_id,
                        );
                    }
                    if publish_render_buffer(&self.render_buffer, &mut self.back_buffer, snapshot) {
                        // VT modes do not change on resize, so the lock-free
                        // atomic remains valid from the last PTY read.
                        self.event_proxy.send_event(
                            TerminalEvent::TerminalDamaged(self.route_id),
                            self.window_id,
                        );
                    }
                    // Resize reflows content → invalidate any deep-search corpus.
                    self.content_version
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if cfg!(windows) {
                        self.conpty_resize_echo_realign = true;
                        self.conpty_resize_at = Some(std::time::Instant::now());
                        self.conpty_resize_repaint_reads_remaining = 8;
                        // Latch the prompt row and size now so SU realignment uses the
                        // pre-resize cursor position.
                        // `active_cursor_row()` is on the prompt row here, but flips
                        // within one frame once ConPTY's first repaint CUP lands, so it
                        // must be captured at resize time, not read live in the storm.
                        self.conpty_resize_prompt_row = active_row.unwrap_or(0);
                        self.conpty_resize_cols = cols;
                        self.conpty_resize_rows = rows;
                        self.su_realign_armed = active_row.is_some();
                    }
                    if let Err(err) = self.pty.set_winsize(window_size) {
                        tracing::warn!("pty set_winsize failed: {err}");
                    }
                }
                Msg::Shutdown => return false,
            }
        }

        true
    }

    #[inline]
    fn pty_write(&mut self, state: &mut PtyState) -> io::Result<()> {
        state.ensure_next();

        'write_many: while let Some(mut current) = state.take_current() {
            'write_one: loop {
                match self.pty.writer().write(current.remaining_bytes()) {
                    Ok(0) => {
                        state.set_current(Some(current));
                        break 'write_many;
                    }
                    Ok(n) => {
                        current.advance(n);
                        if current.finished() {
                            state.goto_next();
                            break 'write_one;
                        }
                    }
                    Err(err) => {
                        state.set_current(Some(current));
                        match err.kind() {
                            ErrorKind::Interrupted | ErrorKind::WouldBlock => break 'write_many,
                            _ => return Err(err),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn channel(&self) -> MsgSender {
        self.sender.clone()
    }

    pub fn spawn(mut self) -> JoinHandle<(Self, PtyState)> {
        spawn_named("PTY reader", move || {
            let mut state = PtyState::default();
            let mut buf = [0u8; READ_BUFFER_SIZE];

            // Token 0 is the reserved `Waker`; PTY source tokens start at 1.
            let mut tokens = (1_usize..).map(Token);

            // Register the PTY sources, handing them the loop `Waker` (Windows soft-ready;
            // ignored on Unix). mio 1.2 is edge-triggered; the loop re-registers interest
            // each pass to pick up write readiness.
            self.pty
                .register(&self.poll, &mut tokens, Interest::READABLE, &self.waker)
                .unwrap();

            let mut events = Events::with_capacity(1024);

            'event_loop: loop {
                // Windows soft-ready is level-like but lives outside the OS poll set,
                // and its worker only wakes on the clear→set edge. A `pty_read` capped
                // by MAX_LOCKED_READ can return with data still in the ring (flag left
                // set), so blocking with `None` would sleep forever on already-signalled
                // data. When a source is still ready, poll with a zero timeout to spin
                // back to `drain_ready` instead of sleeping. Unix returns `false` here
                // and keeps blocking (real OS readiness, re-armed by EPOLL_CTL_MOD).
                events.clear();
                let timeout = if self.pty.has_ready() {
                    Some(std::time::Duration::ZERO)
                } else {
                    self.sync_output_started_at
                        .map(|started| SYNC_OUTPUT_TIMEOUT.saturating_sub(started.elapsed()))
                };
                if let Err(err) = self.poll.poll(&mut events, timeout) {
                    match err.kind() {
                        ErrorKind::Interrupted => continue,
                        _ => {
                            error!("Event loop polling error: {err}");
                            break 'event_loop;
                        }
                    }
                }

                // Drain the `Msg` channel (resize/input/shutdown). It is a plain
                // `std::sync::mpsc` woken via the `Waker` (mio 1.2 has no pollable
                // channel), so it is drained on every wakeup rather than via a token.
                if !self.drain_recv_channel(&mut state) {
                    break;
                }

                // Collect readiness from both sources: the Windows soft-ready set
                // (`drain_ready`) and real `Poll` events (Unix fds; on Windows `poll`
                // only ever yields the waker token). Both feed the same handling below.
                let mut do_read = false;
                let mut do_write = false;
                let mut child_exited = false;
                #[cfg(unix)]
                let mut hup = false;

                for token in self.pty.drain_ready() {
                    if token == self.pty.read_token() {
                        do_read = true;
                    } else if token == self.pty.write_token() {
                        do_write = true;
                    } else if token == self.pty.child_event_token() {
                        child_exited = true;
                    }
                }

                for event in events.iter() {
                    let token = event.token();
                    if token == self.pty.child_event_token() {
                        child_exited = true;
                    } else if token == self.pty.read_token() || token == self.pty.write_token() {
                        #[cfg(unix)]
                        if event.is_read_closed() {
                            hup = true;
                        }
                        if event.is_readable() {
                            do_read = true;
                        }
                        if event.is_writable() {
                            do_write = true;
                        }
                    }
                    // The waker token (and any stray token) needs no handling.
                }

                if child_exited {
                    if let Some(nmt_platform::ChildEvent::Exited) = self.pty.next_child_event() {
                        // Emit `CloseTerminal` directly; PtyPipe owns the event proxy and route id.
                        self.event_proxy.send_event(
                            TerminalEvent::CloseTerminal(self.route_id),
                            self.window_id,
                        );
                        self.event_proxy
                            .send_event(TerminalEvent::Render, self.window_id);
                        break 'event_loop;
                    }
                }

                // Don't do I/O on a dead PTY (Unix HUP / `is_read_closed`).
                #[cfg(unix)]
                let skip_io = hup;
                #[cfg(not(unix))]
                let skip_io = false;

                if !skip_io {
                    // A saturated batch self-wakes, while synchronized output uses the
                    // poll deadline. Either path runs `pty_read` without new PTY bytes
                    // so the pending snapshot can flush (it reads WouldBlock at worst).
                    if do_read || self.snapshot_pending {
                        if let Err(err) = self.pty_read(&mut state, &mut buf) {
                            // On Linux, a `read` on the master side of a PTY can fail
                            // with `EIO` if the client side hangs up. In that case, just
                            // loop back round for the inevitable `Exited` event.
                            #[cfg(target_os = "linux")]
                            if err.raw_os_error() == Some(libc::EIO) {
                                continue;
                            }

                            error!("Error reading from PTY in event loop: {}", err);
                            break 'event_loop;
                        }
                    }

                    if do_write {
                        if let Err(err) = self.pty_write(&mut state) {
                            error!("Error writing to PTY in event loop: {}", err);
                            break 'event_loop;
                        }
                    }
                }

                // Re-register interest if a write is pending (real effect on Unix; the
                // Windows soft-ready path is a no-op).
                let mut interest = Interest::READABLE;
                if state.needs_write() {
                    interest |= Interest::WRITABLE;
                }
                self.pty.reregister(&self.poll, interest).unwrap();
            }

            // The PTY sources are not dropped here, so deregister them explicitly.
            let _ = self.pty.deregister(&self.poll);

            (self, state)
        })
    }
}

#[cfg(test)]
mod prompt_sniffer_tests {
    use super::{PromptRegion, PromptSniffer};

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
        let out = std::cell::RefCell::new(Vec::new());
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
        let log = std::cell::RefCell::new(Vec::<String>::new());
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
        let marks = std::cell::RefCell::new(Vec::<Vec<u8>>::new());
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
        let starts = std::cell::RefCell::new(Vec::<String>::new());
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

    #[test]
    fn right_prompt_marker_stays_in_prompt_region() {
        let stream =
            b"\x1b]133;A\x07left>\x1b]133;P;k=r\x07right\x1b]133;B\x07cmd\x1b]133;C\x07out";
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
}

#[cfg(test)]
mod scrollback_tests {
    use super::scrollback_bytes;

    /// The engine scrollback budget is derived from the config line limit
    /// (not the old hardcoded 10 MB) — proportional to lines × cols, 0 → 0.
    #[test]
    fn scrollback_bytes_from_config() {
        // Default 10k lines @ 80 cols → ~12.8 MB (config-driven, ≈ the old 10 MB).
        assert_eq!(scrollback_bytes(10_000, 80), 10_000 * 80 * 16);
        // Scales with the configured line count.
        assert!(scrollback_bytes(100_000, 80) > scrollback_bytes(10_000, 80));
        // Scales with width (byte budget, not lines).
        assert!(scrollback_bytes(10_000, 200) > scrollback_bytes(10_000, 80));
        // Disabled scrollback → 0 budget.
        assert_eq!(scrollback_bytes(0, 80), 0);
        // No overflow on absurd input.
        assert_eq!(scrollback_bytes(usize::MAX, 80), usize::MAX);
    }
}

#[cfg(test)]
mod ghostty_mirror_tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    use parking_lot::FairMutex;

    use super::{
        Interest, Poll, PtyPipe, PtyState, READ_BUFFER_SIZE, SYNC_OUTPUT_TIMEOUT, Token, Waker,
        max_cup_row_col, mode, publish_render_buffer, rewrite_conpty_resize_echo_cup_rows,
        su_realign_count,
    };
    use crate::event::VoidListener;
    use crate::render_buffer::RenderBuffer;

    #[test]
    fn failed_capture_does_not_publish_back_buffer() {
        let front = FairMutex::new(RenderBuffer::new(2, 1));
        let mut back = RenderBuffer::new(3, 1);

        assert!(!publish_render_buffer(
            &front,
            &mut back,
            Err(crate::ghostty::Error::InvalidValue),
        ));
        assert_eq!(front.lock().cols(), 2);
        assert_eq!(back.cols(), 3);

        assert!(publish_render_buffer(&front, &mut back, Ok(())));
        assert_eq!(front.lock().cols(), 3);
        assert_eq!(back.cols(), 2);
    }

    fn snapshot_row_text(snapshot: &RenderBuffer, y: u16) -> String {
        render_buffer_row_text(snapshot, y as usize)
    }

    fn render_buffer_row_text(buffer: &RenderBuffer, y: usize) -> String {
        (0..buffer.cols())
            .map(|x| {
                let c = buffer.cell(x, y).c();
                if c == '\0' { ' ' } else { c }
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn conpty_resize_echo_rewrites_cup_to_engine_cursor_row() {
        let rewritten = rewrite_conpty_resize_echo_cup_rows(b"\x1b[10;18Hx\x1b[10;19H", 42)
            .expect("CUP row should be rewritten");
        assert_eq!(rewritten, b"\x1b[42;18Hx\x1b[42;19H");
    }

    #[test]
    fn su_realign_count_computes_push_to_align_prompt() {
        // Ghostty keeps the prompt on row 17 while ConPTY repaints it on row 1;
        // (CUP row 2, last CUP). N = 17 − 1 = 16, R_conpty = 1. Repaint fits 80x24.
        let repaint = b"\x1b[2;18Hdir\x1b[2;21H";
        assert_eq!(su_realign_count(repaint, 17, 80, 24, 80, 24), Some((16, 1)));
    }

    #[test]
    fn su_realign_count_uses_last_cup_for_multirow_prompt() {
        // Multi-row wrapped prompt: first CUP row 10 (top), last CUP row 11 (cursor).
        // Anchoring on the LAST CUP (row 11 → R_conpty 10) matches R_ghostty (cursor
        // row 22), so N = 22 − 10 = 12 — not 13, which the first CUP (row 10) would give.
        let repaint = b"\x1b[10;22Hxxx\x1b[11;6H";
        assert_eq!(
            su_realign_count(repaint, 22, 90, 40, 90, 40),
            Some((12, 10))
        );
    }

    #[test]
    fn su_realign_count_none_when_already_aligned() {
        // R_ghostty == R_conpty means no divergence and no SU; the legacy CUP rewrite handles it.
        let repaint = b"\x1b[2;18Hdir\x1b[2;21H";
        assert_eq!(su_realign_count(repaint, 1, 80, 24, 80, 24), None);
    }

    #[test]
    fn su_realign_count_none_on_resize_mismatch() {
        let repaint = b"\x1b[2;18Hdir\x1b[2;21H";
        // engine size moved past the latch → repaint answers a different resize.
        assert_eq!(su_realign_count(repaint, 17, 80, 24, 100, 24), None);
        // repaint addresses col 90, beyond the latched 80 cols → different resize.
        let wide = b"\x1b[2;90Hx\x1b[2;90H";
        assert_eq!(su_realign_count(wide, 17, 80, 24, 80, 24), None);
    }

    #[test]
    fn su_realign_count_none_without_cup() {
        assert_eq!(su_realign_count(b"\x1b[?25l", 17, 80, 24, 80, 24), None);
    }

    #[test]
    fn max_cup_row_col_reports_maxima() {
        assert_eq!(max_cup_row_col(b"\x1b[2;18Hx\x1b[11;6H"), (11, 18));
        assert_eq!(max_cup_row_col(b"no cup here"), (0, 0));
    }

    #[test]
    fn conpty_resize_echo_leaves_wrapped_redraw_when_cursor_already_aligned() {
        // Narrow width: PSReadLine redraws the input across two rows — input starts
        // at row 10, wraps, cursor ends at row 11. ConPTY's cursor row (11) already
        // matches ghostty's cursor row (target 11), so the redraw must be left
        // untouched. Collapsing the first CUP onto row 11 re-wraps the content and
        // makes it accumulate (the cols=37 xxxx-duplication bug).
        let wrapped = b"\x1b[10;22Hxxxxxxxxxxxxxxxxxxxxx\x1b[11;6H";
        assert_eq!(rewrite_conpty_resize_echo_cup_rows(wrapped, 11), None);
    }

    #[test]
    fn conpty_resize_echo_shifts_wrapped_redraw_by_delta_preserving_rows() {
        // Divergent multi-row redraw: ConPTY input spans rows 9-10 (cursor row 10),
        // ghostty's cursor is at row 22. The whole redraw shifts down by 12, keeping
        // the two-row structure intact.
        let rewritten = rewrite_conpty_resize_echo_cup_rows(b"\x1b[9;22Hxxx\x1b[10;6H", 22)
            .expect("divergent redraw should shift");
        assert_eq!(rewritten, b"\x1b[21;22Hxxx\x1b[22;6H");
    }

    struct FakeReader {
        data: Vec<u8>,
    }

    impl std::io::Read for FakeReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.data.is_empty() {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let n = self.data.len().min(buf.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data.drain(..n);
            Ok(n)
        }
    }

    #[derive(Default)]
    struct FakeWriter {
        data: Vec<u8>,
    }

    impl std::io::Write for FakeWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.data.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct FakePty {
        reader: FakeReader,
        writer: FakeWriter,
    }

    impl nmt_platform::ProcessReadWrite for FakePty {
        type Reader = FakeReader;
        type Writer = FakeWriter;

        fn reader(&mut self) -> &mut Self::Reader {
            &mut self.reader
        }

        fn read_token(&self) -> Token {
            Token(1)
        }

        fn writer(&mut self) -> &mut Self::Writer {
            &mut self.writer
        }

        fn write_token(&self) -> Token {
            Token(2)
        }

        fn set_winsize(&mut self, _: nmt_platform::WinsizeBuilder) -> std::io::Result<()> {
            Ok(())
        }

        fn register(
            &mut self,
            _: &Poll,
            _: &mut dyn Iterator<Item = Token>,
            _: Interest,
            _: &std::sync::Arc<Waker>,
        ) -> std::io::Result<()> {
            Ok(())
        }

        fn reregister(&mut self, _: &Poll, _: Interest) -> std::io::Result<()> {
            Ok(())
        }

        fn deregister(&mut self, _: &Poll) -> std::io::Result<()> {
            Ok(())
        }

        fn drain_ready(&self) -> Vec<Token> {
            Vec::new()
        }
    }

    impl nmt_platform::EventedPty for FakePty {
        fn child_event_token(&self) -> Token {
            Token(3)
        }

        fn next_child_event(&mut self) -> Option<nmt_platform::ChildEvent> {
            None
        }
    }

    #[test]
    fn disabled_terminal_responses_are_forwarded_without_replying() {
        let queries = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c";
        let pty = FakePty {
            reader: FakeReader {
                data: queries.to_vec(),
            },
            writer: FakeWriter::default(),
        };
        let mut machine = PtyPipe::new(
            Arc::new(FairMutex::new(RenderBuffer::new(20, 3))),
            Arc::new(AtomicU32::new(0)),
            pty,
            VoidListener {},
            crate::event::WindowId::from(0),
            0,
            nmt_config::colors::Colors::default(),
            1000,
            false,
        )
        .unwrap();
        let forwarded = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let forwarded_sink = Arc::clone(&forwarded);
        machine.set_output_sink(move |bytes| forwarded_sink.lock().extend_from_slice(&bytes));
        machine.set_terminal_responses_enabled(false);

        machine
            .pty_read(&mut PtyState::default(), &mut [0; READ_BUFFER_SIZE])
            .unwrap();

        assert_eq!(&*forwarded.lock(), queries);
        assert!(machine.pty.writer.data.is_empty());
    }

    #[test]
    fn resize_message_publishes_snapshot_to_render_buffer() {
        let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(20, 3)));
        let vt_modes = Arc::new(AtomicU32::new(0));
        let pty = FakePty {
            reader: FakeReader { data: Vec::new() },
            writer: FakeWriter::default(),
        };
        let mut machine = PtyPipe::new(
            Arc::clone(&render_buffer),
            vt_modes,
            pty,
            VoidListener {},
            crate::event::WindowId::from(0),
            0,
            nmt_config::colors::Colors::default(),
            1000,
            false,
        )
        .unwrap();

        {
            let mut engine = machine.ghostty.lock();
            engine.write_vt(b"resize-ok");
        }

        machine
            .sender
            .send(crate::event::Msg::Resize(nmt_platform::WinsizeBuilder {
                rows: 5,
                cols: 20,
                width: 240,
                height: 120,
            }))
            .unwrap();

        let mut state = PtyState::default();
        assert!(machine.drain_recv_channel(&mut state));

        let buffer = render_buffer.lock();
        assert_eq!(buffer.rows(), 5);
        assert_eq!(render_buffer_row_text(&buffer, 0), "resize-ok");
    }

    #[test]
    fn synchronized_output_keeps_published_cursor_on_previous_frame_until_commit() {
        let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(20, 3)));
        let pty = FakePty {
            reader: FakeReader {
                data: b"\x1b[3;1H> input\x1b[3;3H".to_vec(),
            },
            writer: FakeWriter::default(),
        };
        let mut machine = PtyPipe::new(
            Arc::clone(&render_buffer),
            Arc::new(AtomicU32::new(0)),
            pty,
            VoidListener {},
            crate::event::WindowId::from(0),
            0,
            nmt_config::colors::Colors::default(),
            1000,
            false,
        )
        .unwrap();
        let mut state = PtyState::default();
        let mut buf = [0u8; READ_BUFFER_SIZE];

        machine.pty_read(&mut state, &mut buf).unwrap();
        assert_eq!(render_buffer.lock().cursor().row.0, 2);

        machine
            .pty
            .reader
            .data
            .extend_from_slice(b"\x1b[?2026h\x1b[1;1HWorking");
        machine.pty_read(&mut state, &mut buf).unwrap();
        {
            let buffer = render_buffer.lock();
            assert_eq!(
                buffer.cursor().row.0,
                2,
                "an incomplete synchronized frame must not expose its intermediate cursor"
            );
            assert!(
                render_buffer_row_text(&buffer, 0).is_empty(),
                "an incomplete synchronized frame must not expose its intermediate cells"
            );
        }

        machine
            .pty
            .reader
            .data
            .extend_from_slice(b"\x1b[3;3H\x1b[?2026l");
        machine.pty_read(&mut state, &mut buf).unwrap();
        {
            let buffer = render_buffer.lock();
            assert_eq!(buffer.cursor().row.0, 2);
            assert_eq!(render_buffer_row_text(&buffer, 0), "Working");
        }

        machine
            .pty
            .reader
            .data
            .extend_from_slice(b"\x1b[?2026h\x1b[2;1HStuck");
        machine.pty_read(&mut state, &mut buf).unwrap();
        machine.sync_output_started_at = Some(std::time::Instant::now() - SYNC_OUTPUT_TIMEOUT);
        machine.pty_read(&mut state, &mut buf).unwrap();

        {
            let buffer = render_buffer.lock();
            assert_eq!(buffer.cursor().row.0, 1);
            assert_eq!(render_buffer_row_text(&buffer, 1), "Stuck");
        }
        assert!(!machine.ghostty.lock().mode(mode::SYNC_OUTPUT));
    }

    #[test]
    fn conpty_resize_echo_realigns_machine_pty_read_to_cursor_row() {
        let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(134, 42)));
        let vt_modes = Arc::new(AtomicU32::new(0));
        let pty = FakePty {
            reader: FakeReader {
                data: b"\x1b[10;24Hx\x1b[10;25H".to_vec(),
            },
            writer: FakeWriter::default(),
        };
        let mut machine = PtyPipe::new(
            render_buffer,
            vt_modes,
            pty,
            VoidListener {},
            crate::event::WindowId::from(0),
            0,
            nmt_config::colors::Colors::default(),
            1000,
            false,
        )
        .unwrap();

        {
            let mut engine = machine.ghostty.lock();
            engine.write_vt(b"\x1b[2J\x1b[10;1HHISTORY\x1b[42;1HC:\\Workspace\\NiumaTerm>");
        }
        machine.conpty_resize_echo_realign = true;
        machine.conpty_resize_echo_pending = true;

        let mut state = PtyState::default();
        let mut read_buf = [0u8; READ_BUFFER_SIZE];
        machine.pty_read(&mut state, &mut read_buf).unwrap();

        let snapshot = machine.ghostty.lock().snapshot().unwrap();
        assert_eq!(snapshot_row_text(&snapshot, 9), "HISTORY");
        assert_eq!(
            snapshot_row_text(&snapshot, 41),
            "C:\\Workspace\\NiumaTerm>x"
        );
    }

    #[test]
    fn conpty_resize_repaint_realigns_clear_without_new_input() {
        let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(134, 42)));
        let vt_modes = Arc::new(AtomicU32::new(0));
        let pty = FakePty {
            reader: FakeReader {
                data: b"\x1b[1;24H\x1b[J\x1b[1;24HTERMINPUT123\x1b[1;36H".to_vec(),
            },
            writer: FakeWriter::default(),
        };
        let mut machine = PtyPipe::new(
            render_buffer,
            vt_modes,
            pty,
            VoidListener {},
            crate::event::WindowId::from(0),
            0,
            nmt_config::colors::Colors::default(),
            1000,
            false,
        )
        .unwrap();

        {
            let mut engine = machine.ghostty.lock();
            engine.write_vt(b"\x1b[2J\x1b[10;1HHISTORY\x1b[42;1HC:\\Workspace\\NiumaTerm>");
        }
        machine.conpty_resize_echo_realign = true;
        machine.conpty_resize_repaint_reads_remaining = 1;

        let mut state = PtyState::default();
        let mut read_buf = [0u8; READ_BUFFER_SIZE];
        machine.pty_read(&mut state, &mut read_buf).unwrap();

        let snapshot = machine.ghostty.lock().snapshot().unwrap();
        assert_eq!(snapshot_row_text(&snapshot, 9), "HISTORY");
        assert_eq!(
            snapshot_row_text(&snapshot, 41),
            "C:\\Workspace\\NiumaTerm>TERMINPUT123"
        );
    }

    /// When the viewport is scrolled into history, the resize repaint is still
    /// realigned to the ENGINE's active cursor row via `active_cursor_row()` (which is
    /// independent of the scroll pin), so it lands on the active prompt row — not on
    /// the visible history row, and not forced to row 1 (the old 错位 bug, which came
    /// from the viewport-relative `cursor.y` reading 0 while scrolled).
    #[test]
    fn conpty_resize_repaint_realigns_to_active_cursor_when_scrolled() {
        let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(20, 4)));
        let vt_modes = Arc::new(AtomicU32::new(0));
        let pty = FakePty {
            // ConPTY targets active row 3 (its own coords). Differs from the
            // off-screen-cursor target row 1, so the old code would rewrite 3 -> 1.
            reader: FakeReader {
                data: b"\x1b[3;1H\x1b[JINJECT\x1b[3;7H".to_vec(),
            },
            writer: FakeWriter::default(),
        };
        let mut machine = PtyPipe::new(
            render_buffer,
            vt_modes,
            pty,
            VoidListener {},
            crate::event::WindowId::from(0),
            0,
            nmt_config::colors::Colors::default(),
            1000,
            false,
        )
        .unwrap();

        {
            let mut engine = machine.ghostty.lock();
            // Fill well past the 4-row viewport so there is scrollback to pin to.
            engine
                .write_vt(b"\x1b[2JL0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\r\nL7\r\nL8\r\nPROMPT>");
            // Pin the viewport to the top of history: cursor now off-screen.
            engine.scroll_viewport_top();
            let sb = engine.snapshot().unwrap().scrollbar();
            assert!(
                sb.offset < sb.total.saturating_sub(sb.len),
                "precondition: viewport must be scrolled away from the bottom"
            );
        }
        machine.conpty_resize_echo_realign = true;
        machine.conpty_resize_repaint_reads_remaining = 1;

        let mut state = PtyState::default();
        let mut read_buf = [0u8; READ_BUFFER_SIZE];
        machine.pty_read(&mut state, &mut read_buf).unwrap();

        // INJECT must be realigned onto the engine's active cursor row (read
        // independently of the scroll pin), exactly once, never on row 0.
        let (active_row, snapshot) = {
            let mut engine = machine.ghostty.lock();
            let active_row = engine.active_cursor_row().unwrap();
            engine.scroll_viewport_bottom();
            (active_row, engine.snapshot().unwrap())
        };
        assert_eq!(snapshot_row_text(&snapshot, active_row), "INJECT");
        assert_ne!(snapshot_row_text(&snapshot, 0), "INJECT");
        let injects = (0..snapshot.rows() as u16)
            .filter(|&y| snapshot_row_text(&snapshot, y).contains("INJECT"))
            .count();
        assert_eq!(injects, 1, "INJECT must appear exactly once");
    }

    /// Typing while scrolled up: a ConPTY echo of user input is realigned to the
    /// ENGINE's active cursor row (`active_cursor_row()`, independent of the viewport
    /// scroll), so the echo lands on the active prompt row — never on the scrolled-away
    /// history currently in view. ConPTY emits the echo at its own stale CUP row.
    #[test]
    fn conpty_resize_echo_routes_to_active_cursor_when_scrolled_typing() {
        let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(20, 4)));
        let vt_modes = Arc::new(AtomicU32::new(0));
        let pty = FakePty {
            // ConPTY echoes the typed 'Z' at its stale row 2, col 8 (after the
            // prompt in ConPTY's frame). The engine's real prompt is elsewhere.
            reader: FakeReader {
                data: b"\x1b[2;8HZ\x1b[2;9H".to_vec(),
            },
            writer: FakeWriter::default(),
        };
        let mut machine = PtyPipe::new(
            render_buffer,
            vt_modes,
            pty,
            VoidListener {},
            crate::event::WindowId::from(0),
            0,
            nmt_config::colors::Colors::default(),
            1000,
            false,
        )
        .unwrap();

        {
            let mut engine = machine.ghostty.lock();
            engine
                .write_vt(b"\x1b[2JL0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\r\nL7\r\nL8\r\nPROMPT>");
            engine.scroll_viewport_top();
            let sb = engine.snapshot().unwrap().scrollbar();
            assert!(
                sb.offset < sb.total.saturating_sub(sb.len),
                "precondition: viewport scrolled away from the bottom"
            );
        }
        // echo_pending = this read is a ConPTY echo of user input.
        machine.conpty_resize_echo_realign = true;
        machine.conpty_resize_echo_pending = true;

        let mut state = PtyState::default();
        let mut read_buf = [0u8; READ_BUFFER_SIZE];
        machine.pty_read(&mut state, &mut read_buf).unwrap();

        // The echo went to the active screen (off the scrolled-to-top view); scroll
        // to the bottom to observe where it landed.
        let snapshot = {
            let mut engine = machine.ghostty.lock();
            engine.scroll_viewport_bottom();
            engine.snapshot().unwrap()
        };
        // 'Z' lands exactly once, on the prompt row — not a history row.
        let mut z_rows = Vec::new();
        let mut prompt_y = None;
        for y in 0..snapshot.rows() as u16 {
            let text = snapshot_row_text(&snapshot, y);
            if text.contains('Z') {
                z_rows.push(y);
            }
            if text.contains("PROMPT") {
                prompt_y = Some(y);
            }
        }
        assert_eq!(z_rows.len(), 1, "echo 'Z' must appear exactly once");
        assert_eq!(
            Some(z_rows[0]),
            prompt_y,
            "echo 'Z' must land on the prompt row, not a history row"
        );
    }

    // ---- command-blocks: pty_read emits CommandFinished with the launch cwd ----

    #[derive(Clone)]
    struct CollectingListener(Arc<parking_lot::Mutex<Vec<crate::event::TerminalEvent>>>);

    impl crate::event::EventListener for CollectingListener {
        fn event(&self) -> (Option<crate::event::TerminalEvent>, bool) {
            (None, false)
        }

        fn send_event(&self, event: crate::event::TerminalEvent, _: crate::event::WindowId) {
            self.0.lock().push(event);
        }
    }

    /// Drive one `pty_read` over `stream` and return the emitted events plus the
    /// machine (for engine-state assertions).
    fn pty_read_events(
        stream: &[u8],
    ) -> (
        Vec<crate::event::TerminalEvent>,
        PtyPipe<FakePty, CollectingListener>,
    ) {
        let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let pty = FakePty {
            reader: FakeReader {
                data: stream.to_vec(),
            },
            writer: FakeWriter::default(),
        };
        let mut machine = PtyPipe::new(
            Arc::new(FairMutex::new(RenderBuffer::new(80, 24))),
            Arc::new(AtomicU32::new(0)),
            pty,
            CollectingListener(Arc::clone(&events)),
            crate::event::WindowId::from(0),
            0,
            nmt_config::colors::Colors::default(),
            1000,
            true,
        )
        .unwrap();
        let mut state = PtyState::default();
        let mut buf = [0u8; READ_BUFFER_SIZE];
        machine.pty_read(&mut state, &mut buf).unwrap();

        let collected = events.lock().clone();
        (collected, machine)
    }

    fn command_finished_events(stream: &[u8]) -> Vec<crate::event::CommandCapture> {
        let (events, _machine) = pty_read_events(stream);
        events
            .iter()
            .filter_map(|e| match e {
                crate::event::TerminalEvent::CommandFinished(c) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn pty_read_emits_osc_52_clipboard_store() {
        use crate::clipboard::ClipboardType;
        use crate::event::TerminalEvent;

        let (events, _) = pty_read_events(b"\x1b]52;c;Y2xhdWRlLWNvcHk=\x07");
        assert!(events.iter().any(|event| {
            matches!(
                event,
                TerminalEvent::ClipboardStore(ClipboardType::Clipboard, text)
                    if text == "claude-copy"
            )
        }));
    }

    /// Full synthetic session: the ps1's `;A;B;C` prime, the first prompt
    /// (OSC 7 origin + trust-establishing `;D`), then a `cd dest` whose completing
    /// prompt reports OSC 7 dest BEFORE its `;D`. Exactly one block; its cwd is the
    /// launch directory (origin), not the destination; no spurious block-0 for the prime.
    #[test]
    fn pty_read_emits_command_finished_with_launch_cwd() {
        let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]7;file:///C:/origin\x07\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
cd dest\r\n\x1b]133;C\x07\
\x1b]7;file:///C:/dest\x07\x1b]133;D;0\x07\x1b]133;A\x07PS dest> \x1b]133;B\x07";
        let blocks = command_finished_events(stream);
        assert_eq!(
            blocks.len(),
            1,
            "one block for the cd; none for the synthetic prime"
        );
        let b = &blocks[0];
        assert_eq!(b.command, "cd dest");
        assert_eq!(b.exit_code, Some(0));
        assert!(b.started_at <= b.ended_at);
        let cwd = b
            .cwd
            .as_ref()
            .expect("launch cwd latched from OSC 7")
            .to_string_lossy()
            .to_string();
        assert!(
            cwd.contains("origin") && !cwd.contains("dest"),
            "cwd must be the launch (origin) directory, got {cwd}"
        );
    }

    #[test]
    fn pty_read_boundary_protocol_snapshots_clears_then_starts_next_prompt() {
        use crate::event::{BlockEvent, TerminalEvent};

        let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
echo hi\r\n\x1b]133;C\x07hi\r\n\
\x1b]133;D;0\x07\x1b[2J\x1b[3J\x1b[H\x1b]133;A\x07PS> \x1b]133;B\x07";
        let (events, machine) = pty_read_events(stream);

        let mut shape = Vec::new();
        let mut handles = Vec::new();
        for event in &events {
            if let TerminalEvent::BlockBatch(batch) = event {
                for event in batch {
                    match event {
                        BlockEvent::HistoryCleared => shape.push("cleared".into()),
                        BlockEvent::EngineBlock { seq, rows, handle } => {
                            handles.push(*handle);
                            shape.push(format!("block{seq}:{rows}"))
                        }
                        BlockEvent::EngineBlocksSync(live) => {
                            shape.push(format!("sync:{}", live.len()))
                        }
                    }
                }
            }
        }

        assert_eq!(
            shape,
            vec!["block1:2", "sync:1"],
            "the command freezes into one engine block before the protocol clear"
        );

        // The frozen block holds the command's rows, readable via BlockRef.
        {
            let engine = machine.ghostty.lock();
            let block = engine.block_acquire(handles[0]).expect("block alive");
            let text = block.format_range((0, 0), (1, 79), true, true).unwrap();
            assert_eq!(text, "PS> echo hi\nhi");
        }

        let snapshot = machine.ghostty.lock().snapshot().unwrap();
        assert_eq!(snapshot_row_text(&snapshot, 0), "PS>");
        assert!(
            (1..snapshot.rows() as u16).all(|y| snapshot_row_text(&snapshot, y).is_empty()),
            "old command output must not remain in the new block"
        );
    }

    /// A user clear announced by the Clear-Host wrapper (`;K`): the frozen
    /// history drops (HistoryCleared), the engine is wiped, and the session
    /// keeps working — the next command harvests normally from row 0.
    #[test]
    fn pty_read_history_clear_mark_drops_history_and_wipes_engine() {
        use crate::event::{BlockEvent, TerminalEvent};

        let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
echo hi\r\n\x1b]133;C\x07hi\r\n\
\x1b]133;D;0\x07\x1b[2J\x1b[3J\x1b[H\x1b]133;A\x07PS> \x1b]133;B\x07\
clear\r\n\x1b]133;C\x07\x1b]133;K\x07\x1b[2J\x1b[3J\x1b[H";
        let (events, machine) = pty_read_events(stream);

        let cleared_at = events.iter().position(|event| {
            matches!(event, TerminalEvent::BlockBatch(batch)
                if batch.contains(&BlockEvent::HistoryCleared))
        });
        assert!(
            cleared_at.is_some(),
            "the ;K mark must surface HistoryCleared: {events:?}"
        );

        let snapshot = machine.ghostty.lock().snapshot().unwrap();
        assert!(
            (0..snapshot.rows() as u16).all(|y| snapshot_row_text(&snapshot, y).is_empty()),
            "the engine must be wiped at the ;K mark"
        );
    }

    #[test]
    fn pty_read_boundary_protocol_does_not_clear_alt_screen() {
        let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
vim\r\n\x1b]133;C\x07\x1b[?1049hTUI\x1b]133;D;0\x07";
        let (_events, machine) = pty_read_events(stream);

        let mut engine = machine.ghostty.lock();
        assert!(engine.mode(crate::ghostty::mode::ALT_SCREEN));
        let snapshot = engine.snapshot().unwrap();
        let rows: Vec<_> = (0..snapshot.rows() as u16)
            .map(|y| snapshot_row_text(&snapshot, y))
            .collect();
        assert!(
            rows.iter().any(|row| row.contains("TUI")),
            "trusted ;D inside alt screen must not write the block-boundary clear"
        );
    }

    #[test]
    fn pty_read_emits_no_command_for_untrusted_stream() {
        // Out-of-order lifecycle (starts at ;B): untrusted, nothing recorded.
        let blocks =
            command_finished_events(b"\x1b]133;B\x07evil\x1b]133;C\x07out\x1b]133;D;0\x07");
        assert!(blocks.is_empty());
    }

    /// A two-command session produces ordered CommandStarted/CommandFinished pairs with
    /// matching split segment sequence numbers.
    #[test]
    fn pty_read_emits_start_and_finish_events_with_seq() {
        use crate::event::TerminalEvent;
        let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]7;file:///C:/w\x07\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
echo one\r\n\x1b]133;C\x07one\r\n\
\x1b]7;file:///C:/w\x07\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
echo two\r\n\x1b]133;C\x07two\r\n\
\x1b]7;file:///C:/w\x07\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07";
        let (events, machine) = pty_read_events(stream);

        let starts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                TerminalEvent::CommandStarted(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        let finishes: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                TerminalEvent::CommandFinished(c) => Some(c.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts.len(),
            2,
            "one start per real command, none for the prime"
        );
        assert_eq!(finishes.len(), 2);
        assert_eq!(starts[0].command, "echo one");
        assert_eq!(starts[1].command, "echo two");
        assert_eq!(finishes[0].command, "echo one");
        assert_eq!(finishes[1].command, "echo two");
        assert_eq!(starts[0].seq, finishes[0].seq);
        assert_eq!(starts[1].seq, finishes[1].seq);
        assert!(starts[0].seq < starts[1].seq);

        // Mark forwarding is working when the engine tags the
        // prompt rows — the drift-correction ground truth for the view.
        assert!(
            machine.ghostty.lock().has_prompt_tagged_row(),
            "engine rows must carry semantic prompt tags after mark forwarding"
        );
    }
}
