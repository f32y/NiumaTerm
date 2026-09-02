use std::collections::HashSet;
use std::sync::Arc;

use chrono::Local;
use gpui::{Context, FollowMode, Image, ListOffset, px};
use nmt_agent_utils::chat::Item as SessionItem;
use nmt_config::agent::CollapseRows;

use crate::composer::PromptTarget;
use crate::transcript::view::TranscriptView;
use crate::transcript::{compaction_accounting, hidden, is_work_row, should_show_jump_to_latest};

/// A transcript entry plus the local wall-clock time it first appeared
/// (shown on hover) and the turn it belongs to (drives turn folding).
/// Streamed items keep their start time.
pub(crate) struct Entry {
    pub(crate) at: String,
    pub(crate) turn: u64,
    pub(crate) item: SessionItem,
    /// Images a user message carried. Held here rather than on the protocol
    /// item because they are this side's own record: a harness reports what a
    /// message said, not the pixels the person attached to it.
    pub(crate) images: Vec<Arc<Image>>,
}

/// One transcript row for the virtualized list. `PartialEq` powers the
/// render-time diff: kind + indices catch structural changes (fold, collapse,
/// appended rows), the fingerprint catches in-place content changes that move
/// a row's height (streamed text, status flips, detail expansion).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowSpec {
    Entry {
        index: usize,
        fingerprint: u64,
    },
    Work {
        index: usize,
        fingerprint: u64,
    },
    /// The turn's work disclosure, placed above the rows it hides so the
    /// chevron points at its own content.
    TurnFold {
        turn: u64,
        row_count: usize,
        folded: bool,
    },
    /// The turn's closing "Worked for Ns" line. Reporting only; the work it
    /// accounts for is disclosed by [`RowSpec::TurnFold`] further up.
    TurnSummary {
        seconds: u64,
        output_tokens: Option<u64>,
    },
    Interrupted {
        turn: u64,
        output_tokens: Option<u64>,
    },
    RunToggle {
        run_start: usize,
        tool_count: usize,
        expanded: bool,
    },
    /// The live progress line. `compacting` is part of the spec because the
    /// compaction form is a different, taller row, so flipping it has to
    /// remeasure rather than only repaint.
    Working {
        compacting: bool,
    },
}

/// Whether a row belongs to a run of work steps, and so is drawn inside the
/// run's grouping rule. The turn fold heads the whole turn rather than one
/// run, so it stays outside.
pub(crate) fn is_run_row(spec: &RowSpec) -> bool {
    matches!(spec, RowSpec::Work { .. } | RowSpec::RunToggle { .. })
}

/// How much air a row holds below it, as a rank rather than a measurement.
/// What the rhythm is follows from what the rows are, which is what this
/// module decides; how many pixels a rank is worth belongs to the renderer
/// that owns the transcript's geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowGap {
    /// Inside one block: between the steps of a run of work, and under the
    /// disclosure that heads them.
    Step,
    /// Between a turn's work and the prose it is interleaved with. The two
    /// are one answer being assembled, so they are held apart enough to tell
    /// which is which and no further; a reply that alternates a sentence with
    /// a tool call otherwise spends more of the column on air than on text.
    Work,
    /// Between turns: around the prompt that opens one and the line that
    /// closes one. This is the boundary a reader scans for to find where one
    /// exchange ends, so it stays the widest thing in the transcript.
    Group,
}

/// A transcript row together with the space held below it. The gap is part of
/// the compared value because it is part of the row's height: a row whose
/// neighbour changed rank has to be remeasured even though what the row
/// itself says is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptRow {
    pub(crate) spec: RowSpec,
    pub(crate) gap: RowGap,
}

/// Whether a row reports on how the turn was worked rather than on what it
/// said: the steps themselves, the toggle that collapses a run of them, and
/// the disclosure that heads a whole turn's work.
fn is_work_block(spec: &RowSpec) -> bool {
    is_run_row(spec) || matches!(spec, RowSpec::TurnFold { .. })
}

/// Whether a row stands at the edge of a turn. A prompt opens one and the
/// closing line ends one; everything between them is a single answer, however
/// many times it alternates between saying something and doing something.
fn is_turn_edge(items: &[Entry], spec: &RowSpec) -> bool {
    match spec {
        RowSpec::Entry { index, .. } => items
            .get(*index)
            .is_some_and(|entry| matches!(entry.item, SessionItem::UserMessage { .. })),
        RowSpec::TurnSummary { .. } | RowSpec::Interrupted { .. } => true,
        _ => false,
    }
}

/// The space at one boundary between rows. A gap is a property of the
/// boundary rather than of either row: the same work row wants the tight step
/// rhythm above the next step of its run and a wider one above the prose that
/// follows the run, and a rank read off the upper row alone cannot say both.
/// The last row is spaced as though a turn followed it, so gaining a row
/// beneath it leaves its height alone.
fn row_gap(items: &[Entry], above: &RowSpec, below: Option<&RowSpec>) -> RowGap {
    let Some(below) = below else {
        return RowGap::Group;
    };

    if is_work_block(above) && is_run_row(below) {
        return RowGap::Step;
    }

    if (is_work_block(above) || is_work_block(below))
        && !is_turn_edge(items, above)
        && !is_turn_edge(items, below)
    {
        return RowGap::Work;
    }

    RowGap::Group
}

/// Pair every row with its trailing gap, in render order.
fn spaced_rows(items: &[Entry], specs: &[RowSpec]) -> Vec<TranscriptRow> {
    specs
        .iter()
        .enumerate()
        .map(|(ix, spec)| TranscriptRow {
            spec: spec.clone(),
            gap: row_gap(items, spec, specs.get(ix + 1)),
        })
        .collect()
}

/// Where the reader was before something else began moving the transcript for
/// them. The live end is recorded as such rather than as the offset it stands
/// at, because the end moves as the conversation grows.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ReadingPosition {
    Tail,
    At(ListOffset),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnSummary {
    Worked(u64),
    Interrupted,
}

pub(crate) fn turn_summary(interrupted: bool, seconds: Option<u64>) -> Option<TurnSummary> {
    if interrupted {
        Some(TurnSummary::Interrupted)
    } else {
        seconds.map(TurnSummary::Worked)
    }
}

/// Height-relevant signature of a transcript entry. Lengths and small fields
/// instead of hashing full text: O(1) per row per frame, and every real
/// mutation (streamed append, status transition, exit code, expansion) moves
/// at least one component.
pub(crate) fn entry_fingerprint(
    item: &SessionItem,
    detail_expanded: bool,
    annotations_expanded: bool,
) -> u64 {
    let (content_len, status_len, extra) = match item {
        SessionItem::UserMessage { text }
        | SessionItem::AgentMessage { text, .. }
        | SessionItem::Reasoning { summary: text, .. } => {
            (text.as_ref().map_or(0, String::len), 0, 0)
        }
        SessionItem::Error { text } => (text.len(), 0, 0),
        SessionItem::CommandExecution {
            command,
            purpose,
            aggregated_output,
            status,
            exit_code,
            ..
        } => (
            command.len()
                + purpose.as_ref().map_or(0, String::len)
                + aggregated_output.as_ref().map_or(0, String::len),
            status.as_ref().map_or(0, String::len),
            exit_code.map_or(0, |code| (code as u64) ^ (1 << 20)),
        ),
        SessionItem::FileChange {
            paths,
            diff,
            status,
            ..
        } => (
            paths.len() + diff.as_ref().map_or(0, String::len),
            status.as_ref().map_or(0, String::len),
            0,
        ),
        SessionItem::Other {
            kind,
            title,
            output,
            status,
            ..
        } => (
            kind.len() + title.len() + output.as_ref().map_or(0, String::len),
            status.as_ref().map_or(0, String::len),
            0,
        ),
        SessionItem::Compaction { detail, .. } => (
            detail.summary.as_ref().map_or(0, String::len),
            compaction_accounting(detail).len(),
            detail.user_context.as_ref().map_or(0, String::len) as u64,
        ),
    };

    (content_len as u64)
        ^ ((status_len as u64) << 48)
        ^ (extra << 24)
        ^ ((annotations_expanded as u64) << 62)
        ^ ((detail_expanded as u64) << 63)
}

impl TranscriptView {
    pub(crate) fn transcript_has_hidden_content_below(&self) -> bool {
        should_show_jump_to_latest(
            self.transcript_list.is_following_tail(),
            self.transcript_list.is_scrolled_to_end(),
            self.transcript_list.max_offset_for_scrollbar().y,
        )
    }

    /// Re-engaging tail mode also scrolls to the very end (past the last
    /// item), which stays correct while the last row is still growing.
    pub(crate) fn scroll_to_bottom(&self) {
        self.transcript_list.set_follow_mode(FollowMode::Tail);
    }

    /// Append an item and the images it carries, which only a user message
    /// has any of.
    pub(crate) fn push(
        &mut self,
        turn: u64,
        item: SessionItem,
        images: Vec<Arc<Image>>,
        cx: &mut Context<Self>,
    ) {
        // Submitting a user message explicitly returns to the live tail.
        // Agent output preserves a manually chosen reading position via the
        // list's own tail-follow state.
        if matches!(&item, SessionItem::UserMessage { .. }) {
            self.scroll_to_bottom();
        }

        self.items.push(Entry {
            at: Local::now().format("%H:%M").to_string(),
            turn,
            item,
            images,
        });
        cx.notify();
    }

    /// Render one turn: the opening prompt, the work disclosure and the rows
    /// it hides, the final reply, and last the "Worked for Ns" summary.
    /// Running turns render chronologically.
    pub(crate) fn entry_spec(&self, index: usize) -> RowSpec {
        RowSpec::Entry {
            index,
            fingerprint: entry_fingerprint(
                &self.items[index].item,
                self.disclosures.row_expanded(index),
                self.disclosures.annotation_expanded(index),
            ),
        }
    }

    pub(crate) fn work_spec(&self, index: usize) -> RowSpec {
        RowSpec::Work {
            index,
            fingerprint: entry_fingerprint(
                &self.items[index].item,
                self.disclosures.row_expanded(index),
                false,
            ),
        }
    }

    /// Data-only description of every transcript row, in render order. This
    /// is the single source of truth for the transcript's structure; the
    /// virtualized list builds elements only for the visible slice of it.
    pub(crate) fn build_row_specs(&self, collapse: CollapseRows) -> Vec<RowSpec> {
        let mut rows = Vec::new();
        let mut start = 0;

        while start < self.items.len() {
            let turn = self.items[start].turn;
            let mut end = start + 1;

            while end < self.items.len() && self.items[end].turn == turn {
                end += 1;
            }

            self.turn_specs(turn, start, end, collapse, &mut rows);
            start = end;
        }

        // Live progress row, pinned below everything the running turn has
        // produced; replaced by the turn's fold header on completion.
        if self.live_turn.is_working() {
            rows.push(RowSpec::Working {
                compacting: self.live_turn.is_compacting(),
            });
        }

        rows
    }

    pub(crate) fn turn_specs(
        &self,
        turn: u64,
        start: usize,
        end: usize,
        collapse: CollapseRows,
        rows: &mut Vec<RowSpec>,
    ) {
        // How the turn closes, once it has one. A stopped turn is closed by its
        // own marker; otherwise an elapsed-time line, when the session reported
        // a duration at all.
        let summary = turn_summary(
            self.turn_ledger.was_interrupted(turn),
            self.turn_ledger.seconds(turn),
        );

        if summary == Some(TurnSummary::Interrupted) {
            self.stream_specs(start, end, &|_| false, collapse, rows);
            rows.push(RowSpec::Interrupted {
                turn,
                output_tokens: self.turn_ledger.output_tokens(turn),
            });
            return;
        }

        // Running (or pre-thread) turn: plain chronological stream, because its
        // work is what the user is watching happen. Folding keys off the turn
        // having settled rather than off a known duration, so a replayed turn
        // folds too — the transcript file carries no timing for it.
        if !self.turn_ledger.is_settled(turn) {
            self.stream_specs(start, end, &|_| false, collapse, rows);
            return;
        }

        // Only the mode that names work folds a settled turn's work away by
        // default, and only the modes that offer the disclosure can fold at
        // all. "Only tool calls" reads the work inline, so it carries no
        // disclosure and no per-turn toggle; the other two keep the control
        // and record hand-folds against whichever direction their default
        // points.
        let discloses_work = !matches!(collapse, CollapseRows::ToolCalls);
        let folds_by_default = matches!(collapse, CollapseRows::WorkAndToolCalls);
        let folded = discloses_work && folds_by_default != self.toggled_turns.contains(&turn);

        // The final reply stays visible when the turn folds; everything
        // between the prompt and the answer is what the fold hides.
        let final_agent = (start..end).rev().find(|&i| {
            matches!(&self.items[i].item, SessionItem::AgentMessage { .. })
                && !hidden(&self.items[i].item)
        });

        // Only the prompt that opened the turn heads it. A message steered
        // into a turn already in flight was written after part of the reply
        // existed, so hoisting it here would show it above output it never
        // saw; it keeps its place in the stream instead.
        let opening_user =
            (start..end).find(|&i| matches!(&self.items[i].item, SessionItem::UserMessage { .. }));

        if let Some(i) = opening_user {
            rows.push(self.entry_spec(i));
        }

        // What the fold owns: everything the expanded turn shows that the
        // folded one does not. Counting it here keeps the disclosure's label
        // honest and lets a turn with nothing to hide skip the control.
        let folded_away = |i: usize| {
            let item = &self.items[i].item;

            !hidden(item)
                && Some(i) != opening_user
                && Some(i) != final_agent
                && !matches!(
                    item,
                    SessionItem::Error { .. }
                        | SessionItem::Compaction { .. }
                        | SessionItem::UserMessage { .. }
                )
        };
        let row_count = (start..end).filter(|&i| folded_away(i)).count();

        // Above the rows it discloses, so expanding inserts them below the
        // control the user just clicked instead of further up the turn.
        if discloses_work && row_count > 0 {
            rows.push(RowSpec::TurnFold {
                turn,
                row_count,
                folded,
            });
        }

        if folded {
            // Errors, compaction boundaries and steered prompts stay visible
            // inside a folded turn: an error is what the user needs to act on,
            // a boundary marks where the conversation above it stopped being
            // verbatim, and words the user typed are never work to hide.
            for i in start..end {
                let visible_when_folded = match &self.items[i].item {
                    SessionItem::Error { .. } | SessionItem::Compaction { .. } => true,
                    SessionItem::UserMessage { .. } => {
                        Some(i) != opening_user && !hidden(&self.items[i].item)
                    }
                    _ => false,
                };
                if visible_when_folded {
                    rows.push(self.entry_spec(i));
                }
            }
        } else {
            let skip = |i: usize| Some(i) == final_agent || Some(i) == opening_user;
            self.stream_specs(start, end, &skip, collapse, rows);
        }

        if let Some(i) = final_agent {
            rows.push(self.entry_spec(i));
        }

        // The turn's summary closes it, below the answer it accounts for, the
        // same place the interrupted marker sits. A replayed turn reaches here
        // with no duration to state and simply ends after its reply.
        if let Some(TurnSummary::Worked(seconds)) = summary {
            rows.push(RowSpec::TurnSummary {
                seconds,
                output_tokens: self.turn_ledger.output_tokens(turn),
            });
        }
    }

    /// Chronological rows for a slice of the transcript, collapsing runs of
    /// consecutive work-log rows into a "+N tool calls" toggle (unless the
    /// collapse setting is off). Hidden entries are transparent: they neither
    /// render nor split a run.
    pub(crate) fn stream_specs(
        &self,
        start: usize,
        end: usize,
        skip: &dyn Fn(usize) -> bool,
        collapse: CollapseRows,
        rows: &mut Vec<RowSpec>,
    ) {
        let mut i = start;

        while i < end {
            let item = &self.items[i].item;

            if skip(i) || hidden(item) {
                i += 1;
                continue;
            }

            if !is_work_row(item) {
                rows.push(self.entry_spec(i));
                i += 1;
                continue;
            }

            // Extend the run across consecutive (possibly hidden) work rows.
            let run_start = i;
            let mut visible: Vec<usize> = Vec::new();
            let mut j = i;

            while j < end
                && !skip(j)
                && (hidden(&self.items[j].item) || is_work_row(&self.items[j].item))
            {
                if !hidden(&self.items[j].item) {
                    visible.push(j);
                }
                j += 1;
            }

            if !matches!(collapse, CollapseRows::Off) && visible.len() > 1 {
                let expanded = self.disclosures.group_expanded(run_start);

                rows.push(RowSpec::RunToggle {
                    run_start,
                    tool_count: visible.len(),
                    expanded,
                });

                if expanded {
                    for &k in &visible {
                        rows.push(self.work_spec(k));
                    }
                }
            } else {
                for &k in &visible {
                    rows.push(self.work_spec(k));
                }
            }

            i = j;
        }
    }

    /// Diff freshly built specs against the list's current contents and
    /// notify it as narrowly as possible: an equal-count middle means rows
    /// changed in place (streaming growth, expansion) and only needs
    /// remeasuring, which preserves the scroll position exactly; a count
    /// change is a real splice.
    pub(crate) fn sync_transcript_list(&mut self, new: Vec<RowSpec>) {
        let new = spaced_rows(&self.items, &new);

        if self.rows == new {
            return;
        }

        let prefix = self
            .rows
            .iter()
            .zip(&new)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self.rows[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let old_mid = prefix..self.rows.len() - suffix;
        let new_mid = new.len() - suffix - prefix;

        if old_mid.len() == new_mid {
            self.transcript_list.remeasure_items(old_mid);
        } else {
            self.transcript_list.splice(old_mid, new_mid);
        }

        self.rows = new;
    }
}

/// Transcript indices of the prompts that opened a turn, oldest first.
///
/// A branch is cut in front of a whole turn, so only the message that opened
/// one names a cut; a message steered into a turn already running shares that
/// turn with the prompt ahead of it and names nothing of its own.
fn turn_opening_prompts(items: &[Entry]) -> Vec<usize> {
    let mut opened = HashSet::new();

    items
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches!(entry.item, SessionItem::UserMessage { .. }))
        .filter(|(_, entry)| opened.insert(entry.turn))
        .map(|(index, _)| index)
        .collect()
}

impl TranscriptView {
    /// Name the branch point one transcript row points at, for the row's own
    /// menu. `None` where the row is not a prompt that opened a turn, which is
    /// a row no cut can be anchored on.
    pub(crate) fn prompt_target(&self, index: usize) -> Option<PromptTarget> {
        let SessionItem::UserMessage { text: Some(prompt) } = &self.items.get(index)?.item else {
            return None;
        };

        let openings = turn_opening_prompts(&self.items);
        let position = openings.iter().position(|opening| *opening == index)?;

        Some(PromptTarget {
            prompt: prompt.clone(),
            depth: openings.len() - 1 - position,
        })
    }

    /// The transcript row a branch point names, found the way `prompt_target`
    /// names one: counted back from the newest turn-opening prompt, with the
    /// text confirming the count landed on the same message. `None` where the
    /// two disagree, which is a row the transcript should not be moved to.
    pub(crate) fn prompt_row(&self, target: &PromptTarget) -> Option<usize> {
        let openings = turn_opening_prompts(&self.items);
        let index = *openings.get(openings.len().checked_sub(target.depth + 1)?)?;
        let SessionItem::UserMessage { text: Some(prompt) } = &self.items[index].item else {
            return None;
        };
        if *prompt != target.prompt {
            return None;
        }

        self.rows
            .iter()
            .position(|row| matches!(row.spec, RowSpec::Entry { index: at, .. } if at == index))
    }

    /// Put the prompt a branch point names at the top of the transcript, so
    /// the conversation follows the row a picker is highlighting.
    ///
    /// Top rather than merely visible: a picker floats over the bottom of the
    /// transcript, so a prompt revealed at the lower edge would be hidden
    /// behind the list naming it — and the turns the cut would discard are
    /// what the user is deciding about, which is what sits below it.
    pub(crate) fn scroll_to_prompt(
        &self,
        target: &PromptTarget,
        smooth: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.prompt_row(target) else {
            return;
        };

        let offset = ListOffset {
            item_ix: row,
            offset_in_item: px(0.),
        };
        if smooth {
            self.transcript_list.scroll_to_smooth(offset);
        } else {
            self.transcript_list.scroll_to(offset);
        }
        cx.notify();
    }

    /// Hand the transcript to a picker that will scroll it to whichever prompt
    /// it highlights.
    ///
    /// Two things have to happen before it does. The reading position is
    /// remembered, so closing the picker can give it back; and it is pinned,
    /// because the empty space opened below the conversation would otherwise
    /// carry a view that was sitting at the live end down into it.
    ///
    /// The newer hold replaces any older one: a picker that closed by cutting
    /// the conversation rather than by being cancelled leaves its own behind,
    /// and that position describes a conversation the user has since left.
    pub(crate) fn hold_for_picker(&mut self) {
        self.stashed_position = Some(if self.transcript_list.is_following_tail() {
            ReadingPosition::Tail
        } else {
            ReadingPosition::At(self.transcript_list.logical_scroll_top())
        });
        self.transcript_list.freeze_scroll_position();
        self.reserve_below = true;
    }

    /// Take it back, for a picker that closed without changing anything: the
    /// reserved space goes away and the conversation returns to where the
    /// reader left it.
    ///
    /// The return is a jump rather than an eased scroll. Dropping the reserve
    /// shortens what the list can travel in the same frame, so a view sitting
    /// on a prompt near the end is already outside the range an animation
    /// could start from; easing from where it lands after that would read as a
    /// jump followed by a slide.
    pub(crate) fn release_from_picker(&mut self, cx: &mut Context<Self>) {
        self.reserve_below = false;

        match self.stashed_position.take() {
            Some(ReadingPosition::Tail) => self.scroll_to_bottom(),
            Some(ReadingPosition::At(offset)) => self.transcript_list.scroll_to(offset),
            None => {}
        }
        cx.notify();
    }
}
