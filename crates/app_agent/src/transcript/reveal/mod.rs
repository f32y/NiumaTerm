use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{App, Bounds, Context, Div, Pixels, Window, div, px};

use crate::settings::AgentSettings;
use crate::transcript::{RowSpec, TranscriptView};

/// One disclosure in the transcript, as the thing whose opening is animated.
///
/// The three variants key three different collections of expanded state, and
/// a single map over this enum is what lets one toggle path serve all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RevealKey {
    /// A work-log row's detail body, keyed by transcript index.
    Row(usize),
    /// A user message's annotation card, keyed by transcript index.
    Annotation(usize),
    /// A collapsed run of work steps, keyed by the run's first entry.
    Group(usize),
}

/// A piece of the transcript with a height of its own, which is what a height
/// ramp has to run towards.
///
/// A disclosure's block opens inside the row that heads it, while a run's
/// steps open as list rows of their own, so the two are measured apart even
/// when one run toggle is moving all of them at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RevealedPart {
    /// A disclosure's block, opened under its own header.
    Block(RevealKey),
    /// One step of a run, keyed by the entry it draws.
    Step(usize),
}

/// Pieces past this point all start together. A forty-step run staggered the
/// whole way down would take a second to finish arriving, and the reader is
/// already looking at the top of it. Low enough that what the stagger adds to
/// the run's total stays under a third of it: the unrolling has to be visible,
/// and it is the toggle's answer that the reader is waiting on.
const REVEAL_STAGGER_LIMIT: usize = 3;
/// How far revealed content is held above its resting place, in pixels. The
/// content settles downwards, which is the direction the disclosure opened,
/// and lifts back the same way as it shuts.
const REVEAL_RISE: f32 = 4.0;

/// How one kind of disclosure travels: how long each of its pieces takes, and
/// how far apart consecutive pieces start. Every disclosure follows the same
/// curve, so what separates a run from a block is how much time it is given
/// for the distance it covers, not how it spends that time.
#[derive(Clone, Copy)]
struct Motion {
    duration: Duration,
    stagger: Duration,
}

impl Motion {
    /// Longest a disclosure on this motion can still be moving, counted from
    /// its start: the last piece held back, plus its own travel.
    fn window(self) -> Duration {
        self.duration + self.stagger * REVEAL_STAGGER_LIMIT as u32
    }
}

/// A block opening under its own header: one piece, and a tall one. The ramp
/// spends almost the whole distance in the first fifth, so a duration this
/// long still reads as answered immediately, and a block worth hundreds of
/// pixels has enough travel left over to resolve rather than stop dead.
const BLOCK_MOTION: Motion = Motion {
    duration: Duration::from_millis(300),
    stagger: Duration::ZERO,
};

/// A run's steps: a handful of rows worth a couple of dozen pixels each.
///
/// A row covers a fraction of a block's distance, so it is given a fraction
/// of a block's time. The stagger that makes a run unroll from its toggle
/// only exists if it clears a frame, which puts a floor under it, and all of
/// it adds to what the run costs end to end — leaving each row rather less
/// again, so the run still finishes inside a block's span.
const RUN_MOTION: Motion = Motion {
    duration: Duration::from_millis(160),
    stagger: Duration::from_millis(30),
};

impl RevealKey {
    /// How this disclosure's content travels.
    fn motion(self) -> Motion {
        match self {
            Self::Group(_) => RUN_MOTION,
            Self::Row(_) | Self::Annotation(_) => BLOCK_MOTION,
        }
    }
}

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
                let progress = self.progress(key, 0, now);
                let covered = match direction {
                    Direction::Opening => progress,
                    Direction::Closing => 1.0 - progress,
                };

                key.motion().duration.mul_f32(ease_out_inverse(covered))
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

    /// How far along the given piece of a disclosure is, from 0 shut to 1
    /// open. `ordinal` is the piece's position under the disclosure, which is
    /// 0 for content that moves as a single block.
    ///
    /// Content whose disclosure is not moving reports 1: a row the reader
    /// scrolled to long after opening it renders at rest, not mid-entrance.
    pub(crate) fn progress(&self, key: RevealKey, ordinal: usize, now: Instant) -> f32 {
        let Some(reveal) = self.active.get(&key) else {
            return 1.0;
        };
        let motion = key.motion();
        let delay = motion.stagger * ordinal.min(REVEAL_STAGGER_LIMIT) as u32;
        let elapsed = now.saturating_duration_since(reveal.started + delay);
        let ramp = ease_out(elapsed.as_secs_f32() / motion.duration.as_secs_f32());

        match reveal.direction {
            Direction::Opening => ramp,
            Direction::Closing => 1.0 - ramp,
        }
    }

    /// Whether every moving disclosure has finished, which is what decides if
    /// the transcript still needs frames of its own.
    pub(crate) fn settled(&self, now: Instant) -> bool {
        self.active.iter().all(|(key, reveal)| {
            now.saturating_duration_since(reveal.started) >= key.motion().window()
        })
    }

    /// Disclosures that have finished shutting, which is when the content they
    /// were hiding can finally be taken down.
    pub(crate) fn shut(&self, now: Instant) -> Vec<RevealKey> {
        self.active
            .iter()
            .filter(|(_, reveal)| reveal.direction == Direction::Closing)
            .filter(|(key, reveal)| {
                now.saturating_duration_since(reveal.started) >= key.motion().window()
            })
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
/// Every disclosure travels on it. One curve is what makes a run of steps and
/// a block of output opened moments apart read as the same gesture.
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
/// The three expansion sets, the motion clock and the measured heights are one
/// thing at three lifetimes: a click writes the set, the clock runs the ramp
/// the set makes visible, and the height is what that ramp interpolates
/// towards. Taking a disclosure down has to retire all three together, which
/// is why they are held here rather than as five fields on the view.
#[derive(Default)]
pub(crate) struct Disclosures {
    /// Work-log rows whose detail (command output, reasoning text) is
    /// expanded, keyed by transcript index.
    expanded_rows: HashSet<usize>,
    /// Collapsed work-log runs the user has expanded, keyed by the index of
    /// the run's first transcript entry (stable, the list only appends).
    expanded_groups: HashSet<usize>,
    /// User-message annotation cards expanded to show their complete text.
    expanded_annotations: HashSet<usize>,
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

    /// Put the disclosure's content on screen and start it opening. Ending the
    /// motion immediately is what reduced motion asks for.
    pub(crate) fn open(&mut self, key: RevealKey, now: Instant, animate: bool) {
        match key {
            RevealKey::Row(index) => self.expanded_rows.insert(index),
            RevealKey::Annotation(index) => self.expanded_annotations.insert(index),
            RevealKey::Group(run_start) => self.expanded_groups.insert(run_start),
        };

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
    /// the caller drops; `steps` are the run rows whose measured heights leave
    /// the list with the run.
    pub(crate) fn take_down(&mut self, key: RevealKey, steps: &[usize]) -> Option<usize> {
        self.reveals.end(key);
        self.revealed_heights.remove(&RevealedPart::Block(key));

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

                for index in steps {
                    self.revealed_heights.remove(&RevealedPart::Step(*index));
                }

                None
            }
        }
    }

    /// How far through its motion one disclosure is, for the row at `ordinal`
    /// places down it.
    pub(crate) fn progress(&self, key: RevealKey, ordinal: usize, now: Instant) -> f32 {
        self.reveals.progress(key, ordinal, now)
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
        self.reveals.clear();
        self.revealed_heights.clear();
    }

    /// Forget the run expansions and everything measured, which a change to
    /// the collapse setting asks for. The per-row and per-annotation
    /// expansions are not departures from that setting, so they stay.
    pub(crate) fn forget_groups(&mut self) {
        self.expanded_groups.clear();
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
        // A run's steps are measured a row at a time, and those rows leave the
        // list with the run. Their heights are read off the rows still
        // standing, which is why they are collected before the run stops
        // reporting itself as expanded.
        let steps = self.run_steps(key);

        // A closed row's segmented source would otherwise keep a second copy
        // of a large output resident behind a row showing none of it.
        if let Some(index) = self.disclosures.take_down(key, &steps) {
            self.virtual_transcripts.drop_row(index);
        }
    }

    /// Transcript indices of the steps a run toggle currently has on screen.
    fn run_steps(&self, key: RevealKey) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|ix| self.revealed_by(*ix).is_some_and(|(row, _)| row == key))
            .filter_map(|ix| match self.rows[ix].spec {
                RowSpec::Work { index, .. } => Some(index),
                _ => None,
            })
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

    /// The run toggle that put this list row on screen, and how far down the
    /// run the row sits.
    ///
    /// Rows that were already there report `None` and render at rest. A run's
    /// steps follow its toggle contiguously, so walking back over them to the
    /// toggle is what identifies both the disclosure and the row's place under
    /// it without the row specs having to carry either.
    pub(crate) fn revealed_by(&self, ix: usize) -> Option<(RevealKey, usize)> {
        if !matches!(self.rows.get(ix)?.spec, RowSpec::Work { .. }) {
            return None;
        }

        let mut ordinal = 0;
        for cursor in (0..ix).rev() {
            match self.rows[cursor].spec {
                RowSpec::Work { .. } => ordinal += 1,
                RowSpec::RunToggle {
                    run_start,
                    expanded: true,
                    ..
                } => return Some((RevealKey::Group(run_start), ordinal)),
                _ => break,
            }
        }

        None
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
