use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{App, Bounds, Context, Div, Pixels, Window, div, px};

use crate::settings::AgentSettings;
use crate::transcript::{RowSpec, TranscriptView};

/// One disclosure in the transcript, as the thing whose opening is animated.
///
/// The four variants key four different collections of expanded state, and
/// a single map over this enum is what lets one toggle path serve all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RevealKey {
    /// A work-log row's detail body, keyed by transcript index.
    Row(usize),
    /// A user message's annotation card, keyed by transcript index.
    Annotation(usize),
    /// A collapsed run of work steps, keyed by the run's first entry.
    Group(usize),
    /// A settled turn's work, folded behind its "Show work" row, keyed by
    /// turn id.
    Turn(u64),
}

/// A piece of the transcript with a height of its own, which is what a height
/// ramp has to run towards.
///
/// A disclosure's block opens inside the row that heads it, while a run's
/// steps and a turn's folded work open as list rows of their own, so they are
/// measured apart even when one toggle is moving all of them at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RevealedPart {
    /// A disclosure's block, opened under its own header.
    Block(RevealKey),
    /// One list row drawn from one entry: a step of a run, or a reply
    /// written between steps. An entry draws at most one row, so the index
    /// names the row.
    Entry(usize),
    /// A run's toggle, keyed by the run's first entry. Inside a folded turn
    /// the toggle is one of the rows the fold hides; it is keyed apart from
    /// the step drawn for that same entry once the run opens.
    Toggle(usize),
}

/// The list row a piece of the transcript measures as, for the rows that a
/// disclosure can splice in.
pub(crate) fn revealed_part(spec: &RowSpec) -> Option<RevealedPart> {
    match spec {
        RowSpec::Work { index, .. } | RowSpec::Entry { index, .. } => {
            Some(RevealedPart::Entry(*index))
        }
        RowSpec::RunToggle { run_start, .. } => Some(RevealedPart::Toggle(*run_start)),
        _ => None,
    }
}

/// How far revealed content is held above its resting place, in pixels. The
/// content settles downwards, which is the direction the disclosure opened,
/// and lifts back the same way as it shuts.
const REVEAL_RISE: f32 = 4.0;

/// How long any disclosure takes to arrive, and to leave again.
///
/// One duration for every kind of content, so a run of steps and a block of
/// output opened moments apart read as one gesture rather than as two
/// mechanisms. The ramp spends almost the whole distance in the first fifth,
/// so a span this long still reads as answered immediately while leaving a
/// block worth hundreds of pixels enough travel to resolve rather than stop
/// dead. A run's steps start together for the same reason: held back from
/// each other they arrive as a cascade, which is a second gesture on top of
/// the one the reader asked for.
const REVEAL_DURATION: Duration = Duration::from_millis(300);

/// Which way a disclosure is moving.
///
/// Having the pair is what lets one entry serve both halves of the
/// interaction: a disclosure that is shutting reports the same progress
/// running backwards, so every place that reads it — the height of the block,
/// the fade, the chevron's angle — mirrors without knowing a second direction
/// exists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Opening,
    Closing,
}

struct Reveal {
    started: Instant,
    direction: Direction,
}

/// Every disclosure currently moving, and which way.
///
/// An opening entry lives as long as its disclosure stays open, which keeps
/// the row specs stable: dropping it once the motion finished would change
/// what the rows report and cost a remeasure for no visible reason. A settled
/// entry simply reports full progress. A closing entry outlives the click that
/// shut it, because the content it hides has to stay on screen for the exit to
/// have anything to move.
#[derive(Default)]
pub(crate) struct Reveals {
    active: HashMap<RevealKey, Reveal>,
}

impl Reveals {
    /// Start one disclosure opening at `now`. The instant is passed in rather
    /// than read here so a caller that also reports progress does both against
    /// one reading of the clock.
    pub(crate) fn open(&mut self, key: RevealKey, now: Instant) {
        self.start(key, Direction::Opening, now);
    }

    pub(crate) fn close(&mut self, key: RevealKey, now: Instant) {
        self.start(key, Direction::Closing, now);
    }

    /// Set a disclosure moving, resuming from whatever is on screen when it
    /// was already moving the other way. Starting from the far end instead
    /// would snap a half-open block shut before reopening it, which is exactly
    /// what an impatient second click must not produce.
    fn start(&mut self, key: RevealKey, direction: Direction, now: Instant) {
        let resume = match self.active.get(&key) {
            Some(reveal) if reveal.direction != direction => {
                let progress = self.progress(key, now);
                let covered = match direction {
                    Direction::Opening => progress,
                    Direction::Closing => 1.0 - progress,
                };

                REVEAL_DURATION.mul_f32(ease_out_inverse(covered))
            }
            _ => Duration::ZERO,
        };

        self.active.insert(
            key,
            Reveal {
                started: now.checked_sub(resume).unwrap_or(now),
                direction,
            },
        );
    }

    pub(crate) fn end(&mut self, key: RevealKey) {
        self.active.remove(&key);
    }

    pub(crate) fn clear(&mut self) {
        self.active.clear();
    }

    /// How far along a disclosure is, from 0 shut to 1 open. Every piece it
    /// discloses reports the same figure, which is what makes a run of steps
    /// travel as one thing.
    ///
    /// Content whose disclosure is not moving reports 1: a row the reader
    /// scrolled to long after opening it renders at rest, not mid-entrance.
    pub(crate) fn progress(&self, key: RevealKey, now: Instant) -> f32 {
        let Some(reveal) = self.active.get(&key) else {
            return 1.0;
        };
        let elapsed = now.saturating_duration_since(reveal.started);
        let ramp = ease_out(elapsed.as_secs_f32() / REVEAL_DURATION.as_secs_f32());

        match reveal.direction {
            Direction::Opening => ramp,
            Direction::Closing => 1.0 - ramp,
        }
    }

    /// Whether every moving disclosure has finished, which is what decides if
    /// the transcript still needs frames of its own.
    pub(crate) fn settled(&self, now: Instant) -> bool {
        self.active
            .values()
            .all(|reveal| now.saturating_duration_since(reveal.started) >= REVEAL_DURATION)
    }

    /// Disclosures that have finished shutting, which is when the content they
    /// were hiding can finally be taken down.
    pub(crate) fn shut(&self, now: Instant) -> Vec<RevealKey> {
        self.active
            .iter()
            .filter(|(_, reveal)| reveal.direction == Direction::Closing)
            .filter(|(_, reveal)| now.saturating_duration_since(reveal.started) >= REVEAL_DURATION)
            .map(|(key, _)| *key)
            .collect()
    }

    /// Whether this disclosure is on its way out, whatever stage it has
    /// reached.
    pub(crate) fn is_closing(&self, key: RevealKey) -> bool {
        self.active
            .get(&key)
            .is_some_and(|reveal| reveal.direction == Direction::Closing)
    }

    /// Whether this disclosure is part-way through its motion, either way.
    /// An entry that has run its course reports false even though it is kept,
    /// which is what tells a settled disclosure from one still travelling.
    pub(crate) fn moving(&self, key: RevealKey, now: Instant) -> bool {
        self.active
            .get(&key)
            .is_some_and(|reveal| now.saturating_duration_since(reveal.started) < REVEAL_DURATION)
    }

    /// Every disclosure currently shutting, whatever stage it has reached.
    pub(crate) fn closing(&self) -> Vec<RevealKey> {
        self.active
            .iter()
            .filter(|(_, reveal)| reveal.direction == Direction::Closing)
            .map(|(key, _)| *key)
            .collect()
    }
}

/// Exponential ease-out over a parameter clamped to `0..=1`: fastest at the
/// start, asymptotic at the end. Half the distance is gone in a tenth of the
/// time, so content on this curve is where the reader is looking before they
/// can look for it, and finishes by settling rather than by arriving.
///
/// Every disclosure travels on it, over the same span.
fn ease_out(t: f32) -> f32 {
    match t >= 1.0 {
        true => 1.0,
        false => 1.0 - 2f32.powf(-10.0 * t.max(0.0)),
    }
}

/// Where along [`ease_out`] a given share of the distance has been covered,
/// which is the parameter a reversing disclosure has to resume from.
fn ease_out_inverse(covered: f32) -> f32 {
    match covered >= 1.0 {
        true => 1.0,
        false => (-(1.0 - covered.max(0.0)).log2() / 10.0).clamp(0.0, 1.0),
    }
}

/// Which parts of the transcript are open, how far through their motion they
/// are, and how tall each one lays out to.
///
/// The four expansion sets, the motion clock and the measured heights are one
/// thing at three lifetimes: a click writes the set, the clock runs the ramp
/// the set makes visible, and the height is what that ramp interpolates
/// towards. Taking a disclosure down has to retire all three together, which
/// is why they are held here rather than as six fields on the view.
pub(crate) struct Disclosures {
    /// Work-log rows whose detail (command output, reasoning text) is
    /// expanded, keyed by transcript index.
    expanded_rows: HashSet<usize>,
    /// Collapsed work-log runs the user has expanded, keyed by the index of
    /// the run's first transcript entry (stable, the list only appends).
    expanded_groups: HashSet<usize>,
    /// User-message annotation cards expanded to show their complete text.
    expanded_annotations: HashSet<usize>,
    /// Settled turns the user has flipped away from what the collapse setting
    /// does by default: unfolded where it folds a turn's work behind the
    /// "Show work" row, folded where it leaves the work on screen. Recorded
    /// as departures rather than as absolute states because turns keep
    /// settling after the setting was read, and each new one has to take the
    /// default.
    toggled_turns: HashSet<u64>,
    /// Which way that default points, so a turn's disclosure can answer
    /// whether it is open without being handed the setting on every call.
    turns_fold_by_default: bool,
    /// When each moving disclosure started, and which way it is going. This
    /// is what the content it discloses fades and grows against.
    reveals: Reveals,
    /// Full height of each piece of the transcript a disclosure opens,
    /// measured while that piece is on screen. A piece being opened or shut is
    /// drawn inside a box ramped towards this, so the height comes from what
    /// the content actually lays out to rather than being guessed at.
    revealed_heights: HashMap<RevealedPart, Pixels>,
}

impl Disclosures {
    /// Start with nothing open, under a collapse setting that either folds a
    /// settled turn's work by default or leaves it on screen.
    pub(crate) fn new(turns_fold_by_default: bool) -> Self {
        Self {
            expanded_rows: HashSet::new(),
            expanded_groups: HashSet::new(),
            expanded_annotations: HashSet::new(),
            toggled_turns: HashSet::new(),
            turns_fold_by_default,
            reveals: Reveals::default(),
            revealed_heights: HashMap::new(),
        }
    }

    /// Whether the disclosure is open or heading there, which is what its
    /// wording reports: a click that starts an exit has already answered the
    /// reader, whatever is still leaving the screen behind it.
    pub(crate) fn is_disclosing(&self, key: RevealKey) -> bool {
        self.is_disclosed(key) && !self.reveals.is_closing(key)
    }

    /// Whether the disclosure's content is currently part of the transcript,
    /// which stays true through the whole of its exit.
    fn is_disclosed(&self, key: RevealKey) -> bool {
        match key {
            RevealKey::Row(index) => self.expanded_rows.contains(&index),
            RevealKey::Annotation(index) => self.expanded_annotations.contains(&index),
            RevealKey::Group(run_start) => self.expanded_groups.contains(&run_start),
            RevealKey::Turn(turn) => self.turns_fold_by_default == self.turn_toggled(turn),
        }
    }

    pub(crate) fn row_expanded(&self, index: usize) -> bool {
        self.expanded_rows.contains(&index)
    }

    pub(crate) fn annotation_expanded(&self, index: usize) -> bool {
        self.expanded_annotations.contains(&index)
    }

    pub(crate) fn group_expanded(&self, run_start: usize) -> bool {
        self.expanded_groups.contains(&run_start)
    }

    /// Whether the user has flipped this turn away from the collapse
    /// setting's default. The rows are built under that setting, so they read
    /// the departure and apply the default themselves.
    pub(crate) fn turn_toggled(&self, turn: u64) -> bool {
        self.toggled_turns.contains(&turn)
    }

    /// Record whether a turn's work is on screen, as a departure from the
    /// default or a return to it.
    fn set_turn_unfolded(&mut self, turn: u64, unfolded: bool) {
        match unfolded == self.turns_fold_by_default {
            true => self.toggled_turns.insert(turn),
            false => self.toggled_turns.remove(&turn),
        };
    }

    /// Put the disclosure's content on screen and start it opening. Ending the
    /// motion immediately is what reduced motion asks for.
    pub(crate) fn open(&mut self, key: RevealKey, now: Instant, animate: bool) {
        match key {
            RevealKey::Row(index) => {
                self.expanded_rows.insert(index);
            }
            RevealKey::Annotation(index) => {
                self.expanded_annotations.insert(index);
            }
            RevealKey::Group(run_start) => {
                self.expanded_groups.insert(run_start);
            }
            RevealKey::Turn(turn) => self.set_turn_unfolded(turn, true),
        }

        match animate {
            true => self.reveals.open(key, now),
            false => self.reveals.end(key),
        }
    }

    /// Start the exit. The content stays until [`Self::take_down`] runs, which
    /// is what gives the exit something to move.
    pub(crate) fn begin_close(&mut self, key: RevealKey, now: Instant) {
        self.reveals.close(key, now);
    }

    /// Remove a shut disclosure's content and everything measured for it.
    /// Returns the transcript index of a collapsed row, whose segmented source
    /// the caller drops; `parts` are the list rows the disclosure spliced in,
    /// whose measured heights leave the list with them.
    pub(crate) fn take_down(&mut self, key: RevealKey, parts: &[RevealedPart]) -> Option<usize> {
        self.reveals.end(key);
        self.revealed_heights.remove(&RevealedPart::Block(key));

        for part in parts {
            self.revealed_heights.remove(part);
        }

        match key {
            RevealKey::Row(index) => {
                self.expanded_rows.remove(&index);

                Some(index)
            }
            RevealKey::Annotation(index) => {
                self.expanded_annotations.remove(&index);

                None
            }
            RevealKey::Group(run_start) => {
                self.expanded_groups.remove(&run_start);

                None
            }
            RevealKey::Turn(turn) => {
                self.set_turn_unfolded(turn, false);

                None
            }
        }
    }

    /// How far through its motion one disclosure is.
    pub(crate) fn progress(&self, key: RevealKey, now: Instant) -> f32 {
        self.reveals.progress(key, now)
    }

    /// Whether one disclosure is still travelling, either way.
    pub(crate) fn moving(&self, key: RevealKey, now: Instant) -> bool {
        self.reveals.moving(key, now)
    }

    /// The measured full height of one piece, once it has been on screen.
    pub(crate) fn height(&self, part: RevealedPart) -> Option<Pixels> {
        self.revealed_heights.get(&part).copied()
    }

    pub(crate) fn record_height(&mut self, part: RevealedPart, height: Pixels) {
        self.revealed_heights.insert(part, height);
    }

    pub(crate) fn settled(&self, now: Instant) -> bool {
        self.reveals.settled(now)
    }

    /// The disclosures whose exit has finished and whose content can go.
    pub(crate) fn shut(&self, now: Instant) -> Vec<RevealKey> {
        self.reveals.shut(now)
    }

    pub(crate) fn closing(&self) -> Vec<RevealKey> {
        self.reveals.closing()
    }

    /// Forget every expansion and every motion, as a replaced conversation
    /// does.
    pub(crate) fn clear(&mut self) {
        self.expanded_rows.clear();
        self.expanded_groups.clear();
        self.expanded_annotations.clear();
        self.toggled_turns.clear();
        self.reveals.clear();
        self.revealed_heights.clear();
    }

    /// Forget the run expansions, the turn folds and everything measured,
    /// which a change to the collapse setting asks for, and take the default
    /// the new setting folds turns by. The per-row and per-annotation
    /// expansions are not departures from that setting, so they stay.
    pub(crate) fn forget_departures(&mut self, turns_fold_by_default: bool) {
        self.expanded_groups.clear();
        self.toggled_turns.clear();
        self.turns_fold_by_default = turns_fold_by_default;
        self.reveals.clear();
        self.revealed_heights.clear();
    }
}

#[cfg(test)]
impl Disclosures {
    pub(crate) fn expanded_rows(&self) -> &HashSet<usize> {
        &self.expanded_rows
    }

    pub(crate) fn expanded_groups(&self) -> &HashSet<usize> {
        &self.expanded_groups
    }

    pub(crate) fn toggled_turns(&self) -> &HashSet<u64> {
        &self.toggled_turns
    }

    pub(crate) fn expanded_annotations(&self) -> &HashSet<usize> {
        &self.expanded_annotations
    }

    /// Every piece a height has been measured for.
    pub(crate) fn measured_parts(&self) -> Vec<RevealedPart> {
        self.revealed_heights.keys().copied().collect()
    }
}

impl TranscriptView {
    /// Flip one disclosure open or shut, holding the reader's place while its
    /// content changes and setting that content moving either way.
    ///
    /// The list stores its live end as a sentinel meaning "wherever the end
    /// now is", so rows appearing above that sentinel would carry the view
    /// down with them and take the row the reader just clicked out from under
    /// their cursor. Naming the position first pins it. A reader already
    /// sitting at the live end keeps following it, because a conversation
    /// growing under an open disclosure should still scroll itself.
    ///
    /// Shutting leaves the expanded state in place and only starts the exit;
    /// [`Self::settle_shut_disclosures`] takes the content down once there is
    /// no exit left to run.
    pub(crate) fn toggle_disclosure(&mut self, key: RevealKey, cx: &mut Context<Self>) {
        if !self.transcript_list.is_following_tail() {
            self.transcript_list.freeze_scroll_position();
        }

        // Reduced motion is read here rather than where progress is reported:
        // a disclosure that never records a start has nothing in flight, so
        // the motion, the chevron's turn and the frames the transcript asks
        // for all fall away together while the pinning stays.
        let reduce_motion = cx.global::<AgentSettings>().reduce_motion;
        let now = Instant::now();
        // A disclosure part-way through its exit is still on screen but is on
        // its way out, so the click that catches it there is asking for it
        // back rather than asking again for what it is already doing.
        match self.disclosures.is_disclosing(key) {
            true if reduce_motion => self.take_down_disclosure(key),
            true => self.disclosures.begin_close(key, now),
            false => self.disclosures.open(key, now, !reduce_motion),
        }

        cx.notify();
    }

    /// Remove a shut disclosure's content and everything measured or cached
    /// for it. Splitting this from the click is what gives the exit something
    /// to move; by the time it runs there is nothing left on screen to lose.
    pub(crate) fn take_down_disclosure(&mut self, key: RevealKey) {
        // The rows a run or a fold spliced in are measured a row at a time,
        // and those rows leave the list with it. Their heights are read off
        // the rows still standing, which is why they are collected before the
        // disclosure stops reporting itself as open.
        let parts = self.revealed_parts(key);

        // A closed row's segmented source would otherwise keep a second copy
        // of a large output resident behind a row showing none of it.
        if let Some(index) = self.disclosures.take_down(key, &parts) {
            self.virtual_transcripts.drop_row(index);
        }
    }

    /// The list rows a run toggle or a turn fold currently has on screen,
    /// whichever ramp they happen to be travelling on this frame.
    fn revealed_parts(&self, key: RevealKey) -> Vec<RevealedPart> {
        (0..self.rows.len())
            .filter(|ix| match key {
                RevealKey::Group(run_start) => self.run_over(*ix) == Some(run_start),
                RevealKey::Turn(turn) => self.fold_over(*ix) == Some(turn),
                RevealKey::Row(_) | RevealKey::Annotation(_) => false,
            })
            .filter_map(|ix| revealed_part(&self.rows[ix].spec))
            .collect()
    }

    /// Take down every disclosure whose exit has finished, re-pinning the
    /// reading position first.
    ///
    /// The pin from the click has held through the exit, but a run's rows
    /// leave the list here rather than there, and the reader may have scrolled
    /// in between. Naming the position against the layout this frame is built
    /// on is what keeps that removal from moving it.
    pub(crate) fn settle_shut_disclosures(&mut self, now: Instant) {
        let shut = self.disclosures.shut(now);
        if shut.is_empty() {
            return;
        }

        if !self.transcript_list.is_following_tail() {
            self.transcript_list.freeze_scroll_position();
        }

        for key in shut {
            self.take_down_disclosure(key);
        }
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
            (Some(fold), Some(run)) if !self.disclosures.moving(fold, now) => Some(run),
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
        if !self.turn_ledger.is_settled(turn) || !self.hidden_by_fold(ix) {
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
            RowSpec::Entry { index, .. } => !self.survives_fold(index),
            _ => false,
        }
    }

    /// The turn a list row belongs to, for the rows that belong to one.
    fn row_turn(&self, ix: usize) -> Option<u64> {
        match self.rows.get(ix)?.spec {
            RowSpec::Entry { index, .. } | RowSpec::Work { index, .. } => {
                Some(self.items[index].turn)
            }
            RowSpec::RunToggle { run_start, .. } => Some(self.items[run_start].turn),
            RowSpec::TurnFold { turn, .. } | RowSpec::Interrupted { turn, .. } => Some(turn),
            RowSpec::TurnSummary { .. } | RowSpec::Working { .. } => None,
        }
    }
}

/// Open and shut one piece of the transcript by its height, so whatever
/// follows it travels with the content instead of jumping once it is all
/// there.
///
/// While the piece is moving it is taken out of flow and the box around it is
/// what grows, which keeps the height being animated out of what is being
/// measured: the content always lays out at its full size, and the bounds
/// reported back through `view` are the height the ramp runs to. At rest the
/// box goes away entirely, so a row whose output is still streaming grows with
/// it rather than staying pinned to a height measured once.
///
/// `shut_height` is what the box still occupies once the piece has finished
/// shutting, for a piece whose space is taken over by something else at the
/// moment it leaves. Holding that much back makes the two changes cancel, so
/// the removal itself moves nothing.
///
/// A free function rather than a method because the callers build the content
/// out of a borrow of the entry it belongs to, which a second borrow of the
/// view would conflict with.
pub(crate) fn revealed_block(
    body: Div,
    part: RevealedPart,
    progress: f32,
    open_height: Option<Pixels>,
    shut_height: Pixels,
    view: gpui::WeakEntity<TranscriptView>,
) -> Div {
    // Recorded without notifying: the value is read on the next frame of a
    // ramp that is already asking for frames, and a notify raised from inside
    // prepaint would keep the pump awake past the point it settles.
    let measure = move |bounds: Vec<Bounds<Pixels>>, _: &mut Window, cx: &mut App| {
        let Some(height) = bounds.first().map(|bounds| bounds.size.height) else {
            return;
        };

        view.update(cx, |view, _| {
            view.disclosures.record_height(part, height);
        })
        .ok();
    };

    if progress >= 1.0 {
        return div().w_full().on_children_prepainted(measure).child(body);
    }

    div()
        .w_full()
        .relative()
        .h(open_height.map_or(shut_height, |open| {
            open * progress + shut_height * (1.0 - progress)
        }))
        .overflow_hidden()
        .opacity(progress)
        .on_children_prepainted(measure)
        .child(div().absolute().top_0().w_full().child(body))
}

/// Give an element its entrance in place: it fades up to full and settles
/// down from slightly above, and lifts back out the same way as it shuts.
///
/// The rise is an offset on a relatively positioned element, so it moves the
/// content without changing the height around it. This is what content opens
/// with when a clip box cannot hold it — a rounded bubble, whose corner a
/// rectangular clip would square off for as long as the ramp ran.
pub(crate) fn revealed(element: Div, progress: f32) -> Div {
    element
        .relative()
        .top(px(-REVEAL_RISE * (1.0 - progress)))
        .opacity(progress)
}

#[cfg(test)]
mod tests;
