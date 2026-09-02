//! The streamed reply, let onto the screen a character at a time.
//!
//! A backend delivers a reply a few words at a time, and each chunk landing
//! whole makes the text jump in by fits and starts. The typewriter holds a
//! moving edge behind what has arrived and lets the reply through it one
//! character at a time, closing on the streamed text fast enough that the
//! edge never trails it by more than a fraction of a second.

use std::time::{Duration, Instant};

/// How quickly the edge closes on what has arrived: the backlog behind it
/// shrinks by a factor of e over this span. A chunk of forty characters is
/// most of the way on screen a quarter of a second after landing, so the reply
/// reads as being typed rather than as being held back, and once the stream
/// stops the last chunk finishes within a beat.
const CATCH_UP: Duration = Duration::from_millis(125);

/// The slowest the edge moves while anything waits behind it, in characters
/// per second. Closing by a share alone would let the final characters of a
/// chunk trickle in over many frames, which reads as the reply stalling on
/// its last word; the floor keeps that tail arriving at a steady typing pace.
const FLOOR_RATE: f32 = 60.0;

/// The edge of a reply being let onto the screen, and the entry it crosses.
///
/// One reply is typed at a time. A new reply starting while an older one
/// still has text waiting lets the older one land whole: the reader's eye
/// has already moved on to where the new text appears.
pub(crate) struct Typewriter {
    index: usize,
    /// Characters shown, carrying the fraction between frames so a rate
    /// below one character a frame still moves.
    shown: f32,
    ticked: Instant,
}

impl Typewriter {
    /// Start typing entry `index` from `shown` characters, which is what the
    /// entry already had on screen before streamed text reached it.
    pub(crate) fn start(index: usize, shown: usize, now: Instant) -> Self {
        Self {
            index,
            shown: shown as f32,
            ticked: now,
        }
    }

    pub(crate) fn index(&self) -> usize {
        self.index
    }

    /// Move the edge towards `total`, the reply's length in characters as of
    /// now. Returns whether anything is still waiting behind it.
    ///
    /// The step is the larger of the catch-up share and the floor, so a big
    /// chunk closes quickly while a lone character still arrives promptly.
    /// A long gap between frames — a pane that was hidden — closes the whole
    /// backlog at once rather than typing text the reader was not watching.
    pub(crate) fn advance(&mut self, total: usize, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.ticked).as_secs_f32();
        self.ticked = now;

        let total = total as f32;
        let backlog = (total - self.shown).max(0.0);
        let share = 1.0 - (-elapsed / CATCH_UP.as_secs_f32()).exp();
        let step = (backlog * share).max(FLOOR_RATE * elapsed).min(backlog);
        self.shown = (self.shown + step).min(total);

        self.shown < total
    }

    /// How many characters of the reply are on screen.
    pub(crate) fn shown(&self) -> usize {
        self.shown as usize
    }
}

/// The part of `text` an edge `chars` characters in lets through. Counted in
/// characters rather than bytes so a CJK reply types at the same pace as a
/// latin one and the cut never lands inside a character.
pub(crate) fn shown_prefix(text: &str, chars: usize) -> &str {
    text.char_indices()
        .nth(chars)
        .map_or(text, |(end, _)| &text[..end])
}

#[cfg(test)]
mod tests;
