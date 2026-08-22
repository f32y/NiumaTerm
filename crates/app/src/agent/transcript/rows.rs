use std::sync::Arc;

use gpui::Image;

use crate::agent::composer::PromptTarget;
use crate::agent::transcript::view::TranscriptView;
use crate::agent::transcript::{
    compaction_accounting, hidden, is_work_row, should_show_jump_to_latest,
};
use crate::agent::*;

/// A transcript entry plus the local wall-clock time it first appeared
/// (shown on hover) and the turn it belongs to (drives turn folding).
/// Streamed items keep their start time.
pub(in crate::agent) struct Entry {
    pub(in crate::agent) at: String,
    pub(in crate::agent) turn: u64,
    pub(in crate::agent) item: SessionItem,
    /// Images a user message carried. Held here rather than on the protocol
    /// item because they are this side's own record: a harness reports what a
    /// message said, not the pixels the person attached to it.
    pub(in crate::agent) images: Vec<Arc<Image>>,
}

/// One transcript row for the virtualized list. `PartialEq` powers the
/// render-time diff: kind + indices catch structural changes (fold, collapse,
/// appended rows), the fingerprint catches in-place content changes that move
/// a row's height (streamed text, status flips, detail expansion).
#[derive(Clone, PartialEq, Eq)]
pub(in crate::agent) enum RowSpec {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::agent) enum TurnSummary {
    Worked(u64),
    Interrupted,
}

pub(in crate::agent) fn turn_summary(
    interrupted: bool,
    seconds: Option<u64>,
) -> Option<TurnSummary> {
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
pub(in crate::agent) fn entry_fingerprint(
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
    pub(in crate::agent) fn transcript_has_hidden_content_below(&self) -> bool {
        should_show_jump_to_latest(
            self.transcript_list.is_following_tail(),
            self.transcript_list.is_scrolled_to_end(),
            self.transcript_list.max_offset_for_scrollbar().y,
        )
    }

    pub(in crate::agent) fn transcript_has_hidden_content_above(&self) -> bool {
        let logical = self.transcript_list.logical_scroll_top();
        let has_logical_offset = logical.item_ix > 0 || logical.offset_in_item > px(0.);
        // A short bottom-aligned list uses an end anchor even though no content
        // is clipped. The pixel offset distinguishes that alignment sentinel
        // from a genuine logical offset into earlier rows.
        has_logical_offset && self.transcript_list.scroll_px_offset_for_scrollbar().y < px(0.)
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
    pub(in crate::agent) fn entry_spec(&self, index: usize) -> RowSpec {
        RowSpec::Entry {
            index,
            fingerprint: entry_fingerprint(
                &self.items[index].item,
                self.expanded_rows.contains(&index),
                self.expanded_annotations.contains(&index),
            ),
        }
    }

    pub(in crate::agent) fn work_spec(&self, index: usize) -> RowSpec {
        RowSpec::Work {
            index,
            fingerprint: entry_fingerprint(
                &self.items[index].item,
                self.expanded_rows.contains(&index),
                false,
            ),
        }
    }

    /// Data-only description of every transcript row, in render order. This
    /// is the single source of truth for the transcript's structure; the
    /// virtualized list builds elements only for the visible slice of it.
    pub(in crate::agent) fn build_row_specs(&self, collapse: CollapseRows) -> Vec<RowSpec> {
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
        if self.working_started.is_some() {
            rows.push(RowSpec::Working {
                compacting: self.compacting,
            });
        }

        rows
    }

    pub(in crate::agent) fn turn_specs(
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
            self.interrupted_turns.contains(&turn),
            self.completed_turn_seconds.get(&turn).copied(),
        );

        if summary == Some(TurnSummary::Interrupted) {
            self.stream_specs(start, end, &|_| false, collapse, rows);
            rows.push(RowSpec::Interrupted {
                turn,
                output_tokens: self.completed_turn_output_tokens.get(&turn).copied(),
            });
            return;
        }

        // Running (or pre-thread) turn: plain chronological stream, because its
        // work is what the user is watching happen. Folding keys off the turn
        // having settled rather than off a known duration, so a replayed turn
        // folds too — the transcript file carries no timing for it.
        if !self.settled_turns.contains(&turn) {
            self.stream_specs(start, end, &|_| false, collapse, rows);
            return;
        }

        // Only the mode that names work folds a settled turn's work away by
        // default. The other two leave it on screen with the same disclosure
        // above it, so the turn can still be folded by hand — which is what
        // the toggle records, in whichever direction the default points.
        let folds_by_default = matches!(collapse, CollapseRows::WorkAndToolCalls);
        let folded = folds_by_default != self.toggled_turns.contains(&turn);

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
        if row_count > 0 {
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
                output_tokens: self.completed_turn_output_tokens.get(&turn).copied(),
            });
        }
    }

    /// Chronological rows for a slice of the transcript, collapsing runs of
    /// consecutive work-log rows into a "+N tool calls" toggle (unless the
    /// collapse setting is off). Hidden entries are transparent: they neither
    /// render nor split a run.
    pub(in crate::agent) fn stream_specs(
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
                let expanded = self.expanded_groups.contains(&run_start);

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
    pub(in crate::agent) fn sync_transcript_list(&mut self, new: Vec<RowSpec>) {
        if self.row_specs == new {
            return;
        }

        let prefix = self
            .row_specs
            .iter()
            .zip(&new)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self.row_specs[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let old_mid = prefix..self.row_specs.len() - suffix;
        let new_mid = new.len() - suffix - prefix;

        if old_mid.len() == new_mid {
            self.transcript_list.remeasure_items(old_mid);
        } else {
            self.transcript_list.splice(old_mid, new_mid);
        }

        self.row_specs = new;
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
    pub(in crate::agent) fn prompt_target(&self, index: usize) -> Option<PromptTarget> {
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
}
