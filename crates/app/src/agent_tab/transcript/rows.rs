pub(crate) use nmt_agent::transcript::conversation::EntryMetadata as EntryPresentation;

use std::collections::HashSet;

use gpui::{Context, FollowMode, ListOffset, ListState};
use nmt_agent::chat::Item as SessionItem;
use nmt_agent::transcript::TranscriptEntry;
use nmt_config::agent::CollapseRows;

use crate::agent_tab::transcript::compaction_accounting;
use crate::agent_tab::transcript::view::TranscriptView;

pub(crate) type Entry = TranscriptEntry<EntryPresentation>;

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
pub(crate) fn row_gap(items: &[Entry], above: &RowSpec, below: Option<&RowSpec>) -> RowGap {
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
pub(super) fn spaced_rows(items: &[Entry], specs: &[RowSpec], rows: &mut Vec<TranscriptRow>) {
    rows.clear();

    rows.extend(specs.iter().enumerate().map(|(ix, spec)| TranscriptRow {
        spec: spec.clone(),
        gap: row_gap(items, spec, specs.get(ix + 1)),
    }));
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

/// Whether the collapse setting folds a settled turn's work away by default.
/// Only the mode that names work does; the other two leave it on screen.
pub(crate) fn folds_turns(collapse: CollapseRows) -> bool {
    matches!(collapse, CollapseRows::WorkAndToolCalls)
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
        SessionItem::AgentMessage {
            text, questions, ..
        } => (
            text.as_ref().map_or(0, String::len),
            questions.as_ref().map_or(0, Vec::len),
            questions.is_some().into(),
        ),
        SessionItem::UserMessage { text } | SessionItem::Reasoning { summary: text, .. } => {
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

/// Transcript indices of the prompts that opened a turn, oldest first.
///
/// A branch is cut in front of a whole turn, so only the message that opened
/// one names a cut; a message steered into a turn already running shares that
/// turn with the prompt ahead of it and names nothing of its own.
pub(super) fn turn_opening_prompts(items: &[Entry]) -> Vec<usize> {
    let mut opened = HashSet::new();

    items
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches!(entry.item, SessionItem::UserMessage { .. }))
        .filter(|(_, entry)| opened.insert(entry.turn))
        .map(|(index, _)| index)
        .collect()
}

#[derive(Default)]
pub(super) struct PickerReservation {
    /// Reading position from before a picker started scrolling the transcript
    /// to the prompt it highlights, so cancelling that picker returns the
    /// conversation to where the user was reading it.
    pub(super) stashed_position: Option<ReadingPosition>,

    /// A picker is following the transcript, so empty space is left below the
    /// conversation. Without that room a prompt near the end cannot be lifted
    /// clear of the picker: the list stops scrolling once its last row is on
    /// screen, which leaves exactly those prompts behind the list naming
    /// them.
    pub(super) reserve_below: bool,
}

impl PickerReservation {
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
    pub(crate) fn hold_for_picker(&mut self, list: &ListState) {
        self.stashed_position = Some(if list.is_following_tail() {
            ReadingPosition::Tail
        } else {
            ReadingPosition::At(list.logical_scroll_top())
        });

        list.freeze_scroll_position();

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
    pub(crate) fn release_from_picker(
        &mut self,
        list: &ListState,
        cx: &mut Context<TranscriptView>,
    ) {
        self.reserve_below = false;

        match self.stashed_position.take() {
            Some(ReadingPosition::Tail) => list.set_follow_mode(FollowMode::Tail),
            Some(ReadingPosition::At(offset)) => list.scroll_to(offset),
            None => {}
        }

        cx.notify();
    }
}
