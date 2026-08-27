//! Tracks OSC 133 shell lifecycle metadata and OSC 9;4 progress reports.

use std::{env, mem, sync, time};

use memchr::memchr;

use crate::event::{CommandCapture, CommandStart, ProgressReport, ProgressState};

mod command_echo;
mod osc_parse;

use crate::prompt_sniffer::command_echo::render_command_echo;
use crate::prompt_sniffer::osc_parse::{OSC133_MAX, SniffedOsc, parse_sniffed_osc};

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

            if was_trusted
                && !command.is_empty()
                && let Some(started_at) = started_at
            {
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
