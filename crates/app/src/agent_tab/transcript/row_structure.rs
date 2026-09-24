//! The transcript's row structure: which rows a conversation lays out as, in
//! what order, and which disclosure each spliced-in row travels with.
//!
//! Everything here is a read of the conversation, the disclosure state and the
//! typed edge of a streaming reply, so the structure can be rebuilt or queried
//! without touching the list that displays it.

use std::time::Instant;

use gpui::{Pixels, px};
use nmt_agent::chat::Item as SessionItem;
use nmt_agent::transcript::conversation::{ConversationState, hidden};
use nmt_config::agent::CollapseRows;

use crate::agent_tab::transcript::render::gap_px;
use crate::agent_tab::transcript::reveal::{Disclosures, RevealKey, RevealedPart, revealed_part};
use crate::agent_tab::transcript::rows::{
    TranscriptRow, TurnSummary, entry_fingerprint, folds_turns, row_gap, turn_summary,
};
use crate::agent_tab::transcript::typewriter::ReplyTyping;
use crate::agent_tab::transcript::{RowSpec, is_work_row};

/// What the row structure is read from: the conversation, which parts of it
/// are disclosed, and how far a streaming reply has been typed.
#[derive(Clone, Copy)]
pub(crate) struct RowSource<'a> {
    pub(crate) conversation: &'a ConversationState,
    pub(crate) disclosures: &'a Disclosures,
    pub(crate) typing: &'a ReplyTyping,
}

/// The rows the list currently holds, read against the source they were built
/// from, for the questions about which disclosure put a row on screen.
pub(crate) struct RowGeometry<'a> {
    pub(crate) rows: &'a [TranscriptRow],
    pub(crate) source: RowSource<'a>,
}

impl RowSource<'_> {
    /// Rows for every turn of the conversation, followed by the live progress
    /// line while a turn is running.
    #[cfg(test)]
    pub(crate) fn all_specs(&self, collapse: CollapseRows) -> Vec<RowSpec> {
        let mut rows = Vec::new();
        let mut start = 0;

        let items = self.conversation.content.entries();

        while start < items.len() {
            let turn = items[start].turn;

            let mut end = start + 1;

            while end < items.len() && items[end].turn == turn {
                end += 1;
            }

            self.turn_specs(turn, start, end, collapse, &mut rows);

            start = end;
        }

        // Live progress row, pinned below everything the running turn has
        // produced; replaced by the turn's fold header on completion.
        if self.conversation.live.is_working() {
            rows.push(RowSpec::Working {
                compacting: self.conversation.live.is_compacting(),
            });
        }

        rows
    }

    /// Render one turn: the opening prompt, the work disclosure and the rows
    /// it hides, the final reply, and last the "Worked for Ns" summary.
    /// Running turns render chronologically.
    fn entry_spec(&self, index: usize) -> RowSpec {
        let fingerprint = entry_fingerprint(
            &self.conversation.content.entries()[index].item,
            self.disclosures.row_expanded(index),
            self.disclosures.annotation_expanded(index),
        );

        // A reply being typed lays out to the part let through so far, so its
        // signature follows that edge rather than the text behind it. The
        // edge sits above the length bits, which keeps every position of it
        // distinct from every length the text could have.
        let fingerprint = match self.typing.typed_edge(index) {
            Some(shown) => fingerprint ^ ((shown as u64) << 32),
            None => fingerprint,
        };

        RowSpec::Entry { index, fingerprint }
    }

    fn work_spec(&self, index: usize) -> RowSpec {
        RowSpec::Work {
            index,
            fingerprint: entry_fingerprint(
                &self.conversation.content.entries()[index].item,
                self.disclosures.row_expanded(index),
                false,
            ),
        }
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
            self.conversation.turns.was_interrupted(turn),
            self.conversation.turns.seconds(turn),
        );

        if summary == Some(TurnSummary::Interrupted) {
            self.stream_specs(start, end, &|_| false, collapse, rows);

            rows.push(RowSpec::Interrupted {
                turn,
                output_tokens: self.conversation.turns.output_tokens(turn),
            });

            return;
        }

        // Running (or pre-thread) turn: plain chronological stream, because its
        // work is what the user is watching happen. Folding keys off the turn
        // having settled rather than off a known duration, so a replayed turn
        // folds too — the transcript file carries no timing for it.
        if !self.conversation.turns.is_settled(turn) {
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
        let folded = discloses_work && folds_turns(collapse) != self.disclosures.turn_toggled(turn);

        // Only the prompt that opened the turn heads it. A message steered
        // into a turn already in flight was written after part of the reply
        // existed, so hoisting it here would show it above output it never
        // saw; it keeps its place in the stream instead.
        let items = self.conversation.content.entries();

        let opening_user =
            (start..end).find(|&i| matches!(&items[i].item, SessionItem::UserMessage { .. }));

        if let Some(i) = opening_user {
            rows.push(self.entry_spec(i));
        }

        // What the fold owns: everything the expanded turn shows that the
        // folded one does not. Counting it here keeps the disclosure's label
        // honest and lets a turn with nothing to hide skip the control.
        let shown = |i: usize| !hidden(&items[i].item) && Some(i) != opening_user;

        let row_count = (start..end)
            .filter(|&i| shown(i) && !self.survives_fold(i))
            .count();

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
            for i in (start..end).filter(|&i| shown(i) && self.survives_fold(i)) {
                rows.push(self.entry_spec(i));
            }
        } else {
            let skip = |i: usize| Some(i) == opening_user;

            self.stream_specs(start, end, &skip, collapse, rows);
        }

        // The turn's summary closes it, below the answer it accounts for, the
        // same place the interrupted marker sits. A replayed turn reaches here
        // with no duration to state and simply ends after its reply.
        if let Some(TurnSummary::Worked(seconds)) = summary {
            rows.push(RowSpec::TurnSummary {
                seconds,
                output_tokens: self.conversation.turns.output_tokens(turn),
            });
        }
    }

    /// Chronological rows for a slice of the transcript, collapsing runs of
    /// consecutive work-log rows into a "+N tool calls" toggle (unless the
    /// collapse setting is off). Hidden entries are transparent: they neither
    /// render nor split a run.
    fn stream_specs(
        &self,
        start: usize,
        end: usize,
        skip: &dyn Fn(usize) -> bool,
        collapse: CollapseRows,
        rows: &mut Vec<RowSpec>,
    ) {
        let mut i = start;

        let items = self.conversation.content.entries();

        while i < end {
            let item = &items[i].item;

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

            while j < end && !skip(j) && (hidden(&items[j].item) || is_work_row(&items[j].item)) {
                if !hidden(&items[j].item) {
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

    /// Whether an entry stays on screen while its turn is folded. Errors,
    /// compaction boundaries and steered prompts do: an error is what the
    /// user needs to act on, a boundary marks where the conversation above
    /// it stopped being verbatim, and words the user typed are never work to
    /// hide. The final reply does too, selected by identity rather than
    /// moved, because visible events can still arrive after it while the
    /// turn closes; everything between the prompt and that answer is what the
    /// fold hides.
    pub(crate) fn survives_fold(&self, index: usize) -> bool {
        let items = self.conversation.content.entries();
        let entry = &items[index];

        match &entry.item {
            SessionItem::Error { .. }
            | SessionItem::Compaction { .. }
            | SessionItem::UserMessage { .. } => true,
            SessionItem::AgentMessage {
                questions: Some(_), ..
            } => true,
            // The plan as the turn left it stays with the reply that closed
            // the work; the earlier states it passed through are work.
            SessionItem::TaskList { .. } => !items[index + 1..]
                .iter()
                .take_while(|later| later.turn == entry.turn)
                .any(|later| matches!(later.item, SessionItem::TaskList { .. })),
            SessionItem::AgentMessage { .. } => {
                !hidden(&entry.item)
                    && items[index + 1..]
                        .iter()
                        .take_while(|later| later.turn == entry.turn)
                        .all(|later| {
                            hidden(&later.item)
                                || !matches!(later.item, SessionItem::AgentMessage { .. })
                        })
            }
            _ => false,
        }
    }
}

impl RowGeometry<'_> {
    /// The list rows a run toggle or a turn fold currently has on screen,
    /// whichever ramp they happen to be travelling on this frame.
    pub(crate) fn revealed_parts(&self, key: RevealKey) -> Vec<RevealedPart> {
        (0..self.rows.len())
            .filter(|ix| match key {
                RevealKey::Group(run_start) => self.run_over(*ix) == Some(run_start),
                RevealKey::Turn(turn) => self.fold_over(*ix) == Some(turn),
                RevealKey::Row(_) | RevealKey::Annotation(_) => false,
            })
            .filter_map(|ix| revealed_part(&self.rows[ix].spec))
            .collect()
    }

    /// The disclosure whose ramp this list row travels on this frame.
    ///
    /// Rows that were already there report `None` and render at rest. A step
    /// of a run inside an unfolded turn is on screen by two disclosures at
    /// once; it follows the fold while the fold is moving, because the fold is
    /// then moving everything under it, and its run the rest of the time, so
    /// a run opened inside a resting turn still travels.
    pub(crate) fn revealed_by(&self, ix: usize, now: Instant) -> Option<RevealKey> {
        let fold = self.fold_over(ix).map(RevealKey::Turn);
        let run = self.run_over(ix).map(RevealKey::Group);

        match (fold, run) {
            (Some(fold), Some(run)) if !self.source.disclosures.moving(fold, now) => Some(run),
            (Some(fold), _) => Some(fold),
            (None, run) => run,
        }
    }

    /// The run whose expanded toggle put this row on screen. A run's steps
    /// follow its toggle contiguously, so walking back over them to the
    /// toggle is what identifies the run without the row specs having to
    /// carry it.
    fn run_over(&self, ix: usize) -> Option<usize> {
        if !matches!(self.rows.get(ix)?.spec, RowSpec::Work { .. }) {
            return None;
        }

        for cursor in (0..ix).rev() {
            match self.rows[cursor].spec {
                RowSpec::Work { .. } => continue,
                RowSpec::RunToggle {
                    run_start,
                    expanded: true,
                    ..
                } => return Some(run_start),
                _ => break,
            }
        }

        None
    }

    /// The turn whose unfolded "Show work" row put this row on screen.
    ///
    /// The fold heads its turn, and every row of the turn below it that a
    /// folded turn would not show is the fold's. Rows a folded turn keeps —
    /// the final reply, an error, a steered prompt — sit among them and are
    /// walked over, so the work after a steered prompt still finds its fold.
    /// Only a settled turn has one, which spares an unsettled conversation
    /// the walk.
    fn fold_over(&self, ix: usize) -> Option<u64> {
        let turn = self.row_turn(ix)?;

        if !self.source.conversation.turns.is_settled(turn) || !self.hidden_by_fold(ix) {
            return None;
        }

        for cursor in (0..ix).rev() {
            match self.rows[cursor].spec {
                RowSpec::TurnFold {
                    turn: heads,
                    folded: false,
                    ..
                } if heads == turn => return Some(turn),
                _ if self.row_turn(cursor) == Some(turn) => continue,
                _ => break,
            }
        }

        None
    }

    /// Whether this row is one a folded turn would take off the screen.
    fn hidden_by_fold(&self, ix: usize) -> bool {
        match self.rows[ix].spec {
            RowSpec::Work { .. } | RowSpec::RunToggle { .. } => true,
            RowSpec::Entry { index, .. } => !self.source.survives_fold(index),
            _ => false,
        }
    }

    /// The turn a list row belongs to, for the rows that belong to one.
    fn row_turn(&self, ix: usize) -> Option<u64> {
        match self.rows.get(ix)?.spec {
            RowSpec::Entry { index, .. } | RowSpec::Work { index, .. } => {
                Some(self.source.conversation.content.entries()[index].turn)
            }
            RowSpec::RunToggle { run_start, .. } => {
                Some(self.source.conversation.content.entries()[run_start].turn)
            }
            RowSpec::TurnFold { turn, .. } | RowSpec::Interrupted { turn, .. } => Some(turn),
            RowSpec::TurnSummary { .. } | RowSpec::Working { .. } => None,
        }
    }

    /// What a shutting row still occupies once it has finished shutting.
    ///
    /// When a block of rows leaves the list, the row above the block stops
    /// holding its space off the block's first row and starts holding it off
    /// whatever followed the block, and those two boundaries can rank apart:
    /// a toggle sits a step off its first step and a work rank off the reply
    /// after the run. The block's last row holds back exactly that
    /// difference, so the space the row above gains at the removal is the
    /// space the block gives up, and the removal itself moves nothing. Every
    /// row above the last owes nothing, because the boundary it leaves
    /// behind is inside the block.
    pub(crate) fn shut_height(&self, ix: usize, key: RevealKey, now: Instant) -> Pixels {
        if self.revealed_by(ix + 1, now) == Some(key) {
            return px(0.);
        }

        let first = (0..ix)
            .rev()
            .take_while(|cursor| self.revealed_by(*cursor, now) == Some(key))
            .last()
            .unwrap_or(ix);

        let Some(above) = first.checked_sub(1).map(|above| &self.rows[above]) else {
            return px(0.);
        };

        let below = self.rows.get(ix + 1).map(|row| &row.spec);

        let merged = row_gap(
            self.source.conversation.content.entries(),
            &above.spec,
            below,
        );

        px(gap_px(merged) - gap_px(above.gap))
    }
}
