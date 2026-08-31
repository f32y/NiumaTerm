use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::*;

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

/// How long a disclosure takes to arrive. Short enough that a reader opening
/// several rows in a row is never waiting on the previous one, long enough
/// that the arrival registers as motion rather than as a repaint.
const REVEAL_DURATION_MS: u64 = 150;
/// Offset between consecutive rows of a run, so a run reads as unrolling from
/// its toggle downwards.
const REVEAL_STAGGER_MS: u64 = 25;
/// Rows past this point all start together. A forty-step run staggered the
/// whole way down would take a second to finish arriving, and the reader is
/// already looking at the top of it.
const REVEAL_STAGGER_LIMIT: usize = 6;
/// How far revealed content is held above its resting place, in pixels. The
/// content settles downwards, which is the direction the disclosure opened.
const REVEAL_RISE: f32 = 4.0;

const REVEAL_DURATION: Duration = Duration::from_millis(REVEAL_DURATION_MS);
const REVEAL_STAGGER: Duration = Duration::from_millis(REVEAL_STAGGER_MS);
/// Longest a single reveal can still be moving, counted from its start.
const REVEAL_WINDOW: Duration =
    Duration::from_millis(REVEAL_DURATION_MS + REVEAL_STAGGER_MS * REVEAL_STAGGER_LIMIT as u64);

/// When each open disclosure started opening.
///
/// An entry lives as long as its disclosure stays open, which keeps the row
/// specs stable: dropping it once the motion finished would change what the
/// rows report and cost a remeasure for no visible reason. A settled entry
/// simply reports full progress.
#[derive(Default)]
pub(crate) struct Reveals {
    started: HashMap<RevealKey, Instant>,
}

impl Reveals {
    /// Start one disclosure's entrance at `now`. The instant is passed in
    /// rather than read here so a caller that also reports progress does both
    /// against one reading of the clock.
    pub(crate) fn begin(&mut self, key: RevealKey, now: Instant) {
        self.started.insert(key, now);
    }

    pub(crate) fn end(&mut self, key: RevealKey) {
        self.started.remove(&key);
    }

    pub(crate) fn clear(&mut self) {
        self.started.clear();
    }

    /// How far along the given piece of a disclosure is, from 0 at its start
    /// to 1 at rest. `ordinal` is the piece's position under the disclosure,
    /// which is 0 for content that opens as a single block.
    ///
    /// Content whose disclosure is not opening reports 1: a row the reader
    /// scrolled to long after opening it renders at rest, not mid-entrance.
    pub(crate) fn progress(&self, key: RevealKey, ordinal: usize, now: Instant) -> f32 {
        let Some(start) = self.started.get(&key) else {
            return 1.0;
        };
        let delay = REVEAL_STAGGER * ordinal.min(REVEAL_STAGGER_LIMIT) as u32;
        let elapsed = now.saturating_duration_since(*start + delay);

        smoothstep((elapsed.as_secs_f32() / REVEAL_DURATION.as_secs_f32()).clamp(0.0, 1.0))
    }

    /// Whether every open disclosure has finished arriving, which is what
    /// decides if the transcript still needs frames of its own.
    pub(crate) fn settled(&self, now: Instant) -> bool {
        self.started
            .values()
            .all(|start| now.saturating_duration_since(*start) >= REVEAL_WINDOW)
    }
}

impl TranscriptView {
    /// Flip one disclosure open or shut, holding the reader's place while its
    /// content changes and starting the entrance for what it opens.
    ///
    /// The list stores its live end as a sentinel meaning "wherever the end
    /// now is", so rows appearing above that sentinel would carry the view
    /// down with them and take the row the reader just clicked out from under
    /// their cursor. Naming the position first pins it. A reader already
    /// sitting at the live end keeps following it, because a conversation
    /// growing under an open disclosure should still scroll itself.
    pub(crate) fn toggle_disclosure(&mut self, key: RevealKey, cx: &mut Context<Self>) {
        if !self.transcript_list.is_following_tail() {
            self.transcript_list.freeze_scroll_position();
        }

        let opened = {
            let (set, id) = match key {
                RevealKey::Row(index) => (&mut self.expanded_rows, index),
                RevealKey::Annotation(index) => (&mut self.expanded_annotations, index),
                RevealKey::Group(run_start) => (&mut self.expanded_groups, run_start),
            };

            set.insert(id) || {
                set.remove(&id);
                false
            }
        };

        if opened {
            // Reduced motion is read here rather than where progress is
            // reported: a disclosure that never records a start has nothing
            // in flight, so the entrance, the chevron's turn and the frames
            // the transcript asks for all fall away together.
            if !cx.global::<AgentSettings>().reduce_motion {
                self.reveals.begin(key, Instant::now());
            }
        } else {
            self.reveals.end(key);

            // A closed row's segmented source would otherwise keep a second
            // copy of a large output resident behind a row showing none of it.
            if let RevealKey::Row(index) = key {
                self.virtual_transcripts.remove(&index);
            }
        }

        cx.notify();
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

/// Give an element its entrance: it fades up to full and settles down into
/// place from slightly above.
///
/// The rise is an offset on a relatively positioned element, so it moves the
/// content without changing the height the row around it measures. That is
/// what keeps the virtualized list out of the animation: a disclosure reaches
/// its final height on the first frame and is measured once, while only the
/// paint of its content changes across the frames that follow.
pub(crate) fn revealed(element: Div, progress: f32) -> Div {
    element
        .relative()
        .top(px(-REVEAL_RISE * (1.0 - progress)))
        .opacity(progress)
}

#[cfg(test)]
mod tests;
