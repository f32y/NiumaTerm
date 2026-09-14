#[cfg(test)]
#[path = "reveal_tests.rs"]
mod reveal_tests;

use crate::agent_tab::transcript::{RowSpec, TranscriptView};
use gpui::prelude::*;
use gpui::{App, Bounds, Div, Pixels, Window, div, px};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

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

    #[cfg(test)]
    pub(crate) fn expanded_rows(&self) -> &HashSet<usize> {
        &self.expanded_rows
    }

    #[cfg(test)]
    pub(crate) fn expanded_groups(&self) -> &HashSet<usize> {
        &self.expanded_groups
    }

    #[cfg(test)]
    pub(crate) fn toggled_turns(&self) -> &HashSet<u64> {
        &self.toggled_turns
    }

    #[cfg(test)]
    pub(crate) fn expanded_annotations(&self) -> &HashSet<usize> {
        &self.expanded_annotations
    }

    /// Every piece a height has been measured for.
    #[cfg(test)]
    pub(crate) fn measured_parts(&self) -> Vec<RevealedPart> {
        self.revealed_heights.keys().copied().collect()
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
