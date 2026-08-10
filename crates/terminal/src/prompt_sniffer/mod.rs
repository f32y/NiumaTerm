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
mod tests;
