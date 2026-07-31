//! Tracks OSC 133 shell lifecycle metadata and OSC 9;4 progress reports.

use std::{env, mem, str, sync, time};

use memchr::memchr;

use crate::event::{CommandCapture, CommandStart, ProgressReport, ProgressState};

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
const OSC_PROGRESS_PREFIX: &[u8] = b"\x1b]9;4;";
/// Max bytes of one mark we buffer/scan (`ESC]133;D;<exit>ST` is far shorter). A malformed
/// mark longer than this resyncs as ordinary bytes; also bounds the inline carry buffer.
const OSC133_MAX: usize = 32;

/// Outcome of scanning an ESC in the PTY stream for sequences the sniffer
/// cares about: OSC 133 prompt marks and OSC 9;4 progress reports.
enum SniffedOsc {
    /// A complete mark: consume `len` bytes; `next` is the region to switch to (`None` =
    /// a recognized 133 mark with no transition, e.g. `;P` right-prompt). `exit` is the
    /// command exit code carried by a `;D;<code>` mark (`None` for a bare `;D`).
    Mark {
        len: usize,
        sub: u8,
        exit: Option<i32>,
        next: Option<PromptRegion>,
    },
    Progress {
        len: usize,
        report: ProgressReport,
    },
    /// Looks like the start of a 133 mark but the slice ends before the terminator — carry.
    Incomplete,
    /// Not a 133 mark; the ESC is an ordinary terminal byte.
    NotMark,
    /// Starts like OSC 133 but is malformed or too long.
    Malformed,
    /// Starts like OSC 9;4 but is malformed or too long.
    ProgressMalformed,
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
    str::from_utf8(digits).ok()?.parse().ok()
}

/// Parse a potential OSC 133 mark at `s` (caller guarantees `s[0] == 0x1b`).
fn parse_sniffed_osc(s: &[u8]) -> SniffedOsc {
    let progress_prefix_len = s.len().min(OSC_PROGRESS_PREFIX.len());

    if s[..progress_prefix_len] == OSC_PROGRESS_PREFIX[..progress_prefix_len] {
        if s.len() <= OSC_PROGRESS_PREFIX.len() + 1 {
            return SniffedOsc::Incomplete;
        }

        let state = match s[OSC_PROGRESS_PREFIX.len()] {
            b'0' => ProgressState::Remove,
            b'1' => ProgressState::Set,
            b'2' => ProgressState::Error,
            b'3' => ProgressState::Indeterminate,
            b'4' => ProgressState::Pause,
            _ => return SniffedOsc::ProgressMalformed,
        };

        if s[OSC_PROGRESS_PREFIX.len() + 1] != b';' {
            return SniffedOsc::ProgressMalformed;
        }

        let end = s.len().min(OSC133_MAX);
        let arg_start = OSC_PROGRESS_PREFIX.len() + 2;
        let mut i = arg_start;

        // The percentage argument. Values above 100 are clamped rather than
        // rejected, so a miscounting emitter still gets a full bar.
        let percent = |term: usize| {
            str::from_utf8(&s[arg_start..term])
                .ok()?
                .parse::<u32>()
                .ok()
                .map(|value| value.min(100) as u8)
        };

        while i < end {
            match s[i] {
                0x07 => {
                    return SniffedOsc::Progress {
                        len: i + 1,
                        report: ProgressReport {
                            state,
                            progress: percent(i),
                        },
                    };
                }
                0x1b if i + 1 < s.len() && s[i + 1] == b'\\' => {
                    return SniffedOsc::Progress {
                        len: i + 2,
                        report: ProgressReport {
                            state,
                            progress: percent(i),
                        },
                    };
                }
                0x1b if i + 1 == s.len() => return SniffedOsc::Incomplete,
                0x1b => return SniffedOsc::ProgressMalformed,
                _ => i += 1,
            }
        }

        return if s.len() < OSC133_MAX {
            SniffedOsc::Incomplete
        } else {
            SniffedOsc::ProgressMalformed
        };
    }

    let pn = s.len().min(OSC133_PREFIX.len());

    if s[..pn] != OSC133_PREFIX[..pn] {
        return SniffedOsc::NotMark;
    }

    if s.len() <= OSC133_PREFIX.len() {
        return SniffedOsc::Incomplete; // prefix matched so far; need the subcommand + terminator
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
                return SniffedOsc::Mark {
                    len: i + 1,
                    sub,
                    exit: exit_at(i),
                    next: region_for(sub),
                };
            }
            0x1b if i + 1 < s.len() && s[i + 1] == 0x5c => {
                return SniffedOsc::Mark {
                    len: i + 2,
                    sub,
                    exit: exit_at(i),
                    next: region_for(sub),
                };
            }
            0x1b if i + 1 == s.len() => return SniffedOsc::Incomplete, // maybe a split ST
            0x1b => return SniffedOsc::Malformed,                      // ESC mid-mark that isn't ST
            _ => i += 1,
        }
    }

    if s.len() < OSC133_MAX {
        SniffedOsc::Incomplete // a terminator may still arrive next read
    } else {
        SniffedOsc::Malformed // too long, resync on the ESC
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
    /// Active OSC 9;4 progress suppresses only the published cursor, leaving the
    /// engine's DECTCEM state intact for exact restoration on removal.
    progress_active: bool,
    boundary_trust: ShellBoundaryTrust,
    boundary_trust_changed: Option<bool>,
    lifecycle: ShellLifecycleProgress,
    carry: [u8; OSC133_MAX],
    carry_len: usize,
    /// Accumulates the current command-echo region's raw bytes (`;B`→`;C`), mirroring
    /// the shell lifecycle. Cleared at each `;B`; never grows on the output hot path.
    command_buf: Vec<u8>,
    /// When command output started (`;C`) — the block's `started_at`.
    command_started_at: Option<time::SystemTime>,
    /// The command echo rendered once at `;C` (`render_command_echo`); reused by the
    /// `;D` capture so the echo emulation runs once per command.
    current_command: String,
    /// Edge flags/payloads set by `apply` and drained by `feed_hooked` into the
    /// `on_mark` hook at the mark's exact stream position.
    prompt_start_edge: bool,
    command_started_edge: bool,
    command_finished: Option<CommandCapture>,
    /// The OSC 9;4 report just parsed, surfaced so the tab strip can draw it.
    progress_edge: Option<ProgressReport>,
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
    /// An OSC 9;4 progress report (`ESC ] 9 ; 4 ; <state> ; <percent>`).
    pub progress: Option<ProgressReport>,
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
    pub(crate) fn feed_hooked(
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

            match parse_sniffed_osc(&tmp[..cl + take]) {
                SniffedOsc::Mark {
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
                SniffedOsc::Progress { len, report } => {
                    self.note_progress(report);

                    let mark = self.drain_mark(&tmp[..len]);

                    on_mark(mark);

                    self.carry_len = 0;

                    pos = len - cl;
                }
                SniffedOsc::Incomplete if cl + take < OSC133_MAX => {
                    self.carry[cl..cl + take].copy_from_slice(&input[..take]);

                    self.carry_len = cl + take;

                    return; // still incomplete — wait for the next read
                }
                SniffedOsc::NotMark | SniffedOsc::ProgressMalformed => {
                    // The carried ESC resolved to an ordinary escape split across
                    // reads (any chunk ending in "\x1b" or "\x1b]…" lands here —
                    // constant under ESC-dense output like vtebench). Forward the
                    // carried bytes and rescan the new input; NOT a boundary
                    // glitch, so trust is untouched.
                    let mut tmp2 = [0u8; OSC133_MAX];

                    tmp2[..cl].copy_from_slice(&self.carry[..cl]);

                    // Same as the main-scan path: carried bytes inside the
                    // command region must also land in command_buf, or a
                    // read boundary through an ordinary escape drops
                    // characters from the captured command text.
                    self.pre_forward(&tmp2[..cl]);

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
            match memchr(0x1b, &input[pos..]) {
                None => break,
                Some(off) => {
                    let esc = pos + off;

                    match parse_sniffed_osc(&input[esc..]) {
                        SniffedOsc::Mark {
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
                        SniffedOsc::Progress { len, report } => {
                            if esc > seg_start {
                                self.pre_forward(&input[seg_start..esc]);

                                forward(
                                    self.region,
                                    self.boundary_trusted(),
                                    &input[seg_start..esc],
                                );
                            }

                            self.note_progress(report);

                            let mark = self.drain_mark(&input[esc..esc + len]);

                            on_mark(mark);

                            pos = esc + len;

                            seg_start = pos;
                        }
                        SniffedOsc::Incomplete => {
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
                        SniffedOsc::NotMark | SniffedOsc::ProgressMalformed => pos = esc + 1,
                        SniffedOsc::Malformed => {
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
            prompt_started: mem::take(&mut self.prompt_start_edge),
            command_started: mem::take(&mut self.command_started_edge).then(|| CommandStart {
                seq: 0, // stamped by the mark-closure caller (block-split)
                command: self.current_command.clone(),
                cwd: None,
                started_at: self
                    .command_started_at
                    .unwrap_or_else(time::SystemTime::now),
            }),
            command_finished: self.command_finished.take(),
            progress: self.progress_edge.take(),
            history_cleared: mem::take(&mut self.history_cleared_edge),
        }
    }

    /// Latch an OSC 9;4 report for the mark being drained. `progress_active`
    /// gates cursor publication; the report itself rides out to the tab strip.
    fn note_progress(&mut self, report: ProgressReport) {
        self.progress_active = report.state != ProgressState::Remove;

        self.progress_edge = Some(report);
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
            self.command_started_at = Some(time::SystemTime::now());
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
            let command = mem::take(&mut self.current_command);

            self.command_buf.clear();

            if was_trusted && !command.is_empty() {
                if let Some(started_at) = started_at {
                    self.command_finished = Some(CommandCapture {
                        seq: 0, // stamped by the mark-closure caller (block-split)
                        command,
                        exit_code: exit,
                        cwd: None, // filled by the caller from its ;C latch
                        started_at,
                        ended_at: time::SystemTime::now(),
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

    pub(crate) fn progress_active(&self) -> bool {
        self.progress_active
    }

    pub(crate) fn take_boundary_trust_changed(&mut self) -> Option<bool> {
        self.boundary_trust_changed.take()
    }
}

/// Gated on `NMT_PROMPT_TRACE` so live OSC 133 region transitions can be logged during
/// classify-only validation; zero-cost when unset (one `OnceLock` load).
fn prompt_trace_enabled() -> bool {
    static EN: sync::OnceLock<bool> = sync::OnceLock::new();
    *EN.get_or_init(|| env::var_os("NMT_PROMPT_TRACE").is_some())
}

#[cfg(test)]
mod prompt_sniffer_tests {
    use std::cell;

    use super::{
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
