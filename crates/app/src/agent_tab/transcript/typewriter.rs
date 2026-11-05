#[cfg(test)]
#[path = "typewriter_tests.rs"]
mod tests;

use std::time::{Duration, Instant};

use nmt_agent::chat::Item;
use nmt_agent::transcript::conversation::{ContentChange, EntryChange};

use crate::agent_tab::transcript::Entry;

/// The reply this view types, while there is one. Only text that streams in
/// through the view is typed: a restored or mirrored conversation arrives
/// whole and is shown whole.
pub(super) struct ReplyTyping {
    current: Option<Typewriter>,
}

/// One frame of typing: which reply the edge crosses, whether that reply's
/// row has to be laid out again, and whether text still waits behind the edge.
pub(super) struct TypingStep {
    pub(super) index: usize,
    pub(super) remeasure: bool,
    pub(super) moving: bool,
}

impl ReplyTyping {
    pub(super) fn new() -> Self {
        Self { current: None }
    }

    pub(super) fn is_typing(&self) -> bool {
        self.current.as_ref().is_some_and(Typewriter::moving)
    }

    pub(super) fn frame_delay(&self, now: Instant, interval: Duration) -> Option<Duration> {
        self.current
            .as_ref()
            .filter(|current| current.moving())
            .map(|current| interval.saturating_sub(now.saturating_duration_since(current.ticked)))
    }

    pub(super) fn sync(
        &mut self,
        change: &ContentChange,
        entries: &[Entry],
        now: Instant,
    ) -> Option<usize> {
        if let Some(current) = &mut self.current {
            if change
                .reset_from
                .is_some_and(|first| first <= current.index)
            {
                self.current = None;
            } else if let Some((_, update)) = change
                .entries
                .iter()
                .find(|(index, _)| *index == current.index)
            {
                match entries.get(current.index).map(|entry| &entry.item) {
                    Some(Item::AgentMessage {
                        text: Some(text), ..
                    }) => {
                        current.update(text, *update, now);
                    }
                    _ => self.current = None,
                }
            }
        }

        let (index, previous_bytes) = change.reply?;

        let Item::AgentMessage {
            text: Some(text), ..
        } = &entries.get(index)?.item
        else {
            return None;
        };

        let previous_bytes = change
            .entries
            .iter()
            .find_map(|(entry, update)| match update {
                EntryChange::Append { previous_bytes } if *entry == index => Some(*previous_bytes),
                _ => None,
            })
            .unwrap_or(previous_bytes);

        self.begin(index, text, previous_bytes, now)
    }

    /// Type reply `index` from here on, starting after the `previous_bytes`
    /// of `text` that were on screen before the newest delta. A reply already
    /// being typed keeps its edge. Returns an older reply that stops being
    /// typed and appears whole, whose row has to be laid out again.
    fn begin(
        &mut self,
        index: usize,
        text: &str,
        previous_bytes: usize,
        now: Instant,
    ) -> Option<usize> {
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.index == index)
        {
            return None;
        }

        let previous = self.current.as_ref().map(Typewriter::index);

        // The offset was recorded against the text before the newest delta. A
        // completed payload merged in the same batch can replace that text with
        // a shorter or differently encoded one, so the offset is clamped to a
        // character boundary of the text as it is now.
        self.current = Some(Typewriter::start(index, text, previous_bytes, now));

        previous
    }

    /// Stop typing and show the whole reply. Returns the reply whose row has
    /// to be laid out again.
    pub(super) fn finish(&mut self) -> Option<usize> {
        let current = self.current.as_mut()?;
        let changed = current.moving();

        current.shown = current.total_chars as f64;
        current.shown_bytes = current.total_bytes;

        changed.then_some(current.index)
    }

    /// The part of reply `index` the reader sees this frame: the whole of it
    /// unless its edge is still crossing the text.
    pub(super) fn shown_reply<'a>(&self, index: usize, text: &'a str) -> &'a str {
        match self
            .current
            .as_ref()
            .filter(|current| current.index == index)
        {
            Some(current) => &text[..text.floor_char_boundary(current.shown_bytes)],
            None => text,
        }
    }

    /// How many characters of reply `index` are on screen, while that reply
    /// is the one being typed.
    pub(super) fn typed_edge(&self, index: usize) -> Option<usize> {
        self.current
            .as_ref()
            .filter(|typewriter| typewriter.index() == index && typewriter.moving())
            .map(Typewriter::shown)
    }

    /// Advance the byte offset through newly displayed characters only.
    pub(super) fn advance<'a>(
        &mut self,
        now: Instant,
        reply_text: impl FnOnce(usize) -> &'a str,
    ) -> Option<TypingStep> {
        let typewriter = self.current.as_mut()?;
        let index = typewriter.index();
        let previous = typewriter.shown();

        let moving = typewriter.advance(reply_text(index), now);
        let remeasure = typewriter.shown() != previous || !moving;

        Some(TypingStep {
            index,
            remeasure,
            moving,
        })
    }
}

// The streamed reply, let onto the screen a character at a time.
//
// A backend delivers a reply a few words at a time, and each chunk appearing
// whole makes the text jump in by fits and starts. The typewriter holds a
// moving edge behind what has arrived and lets the reply through it one
// character at a time, closing on the streamed text fast enough that the
// edge never trails it by more than a fraction of a second.

/// How quickly the edge closes on what has arrived: the backlog behind it
/// shrinks by a factor of e over this span. A chunk of forty characters is
/// most of the way on screen a quarter of a second after arriving, so the reply
/// reads as being typed instead of as being held back, and once the stream
/// stops the last chunk finishes within a beat.
const CATCH_UP: Duration = Duration::from_millis(125);

/// The slowest the edge moves while anything waits behind it, in characters
/// per second. Closing by a share alone would let the final characters of a
/// chunk trickle in over many frames, which reads as the reply stalling on
/// its last word; the floor keeps that tail arriving at a steady typing pace.
const FLOOR_RATE: f64 = 60.0;

/// The edge of a reply being let onto the screen, and the entry it crosses.
///
/// One reply is typed at a time. A new reply starting while an older one
/// still has text waiting lets the older one land whole: the reader's eye
/// has already moved on to where the new text appears.
struct Typewriter {
    index: usize,

    /// Characters shown, keeping the fraction between frames so a rate
    /// below one character a frame still moves.
    shown: f64,

    shown_bytes: usize,
    total_chars: usize,
    total_bytes: usize,

    #[cfg(test)]
    scanned_bytes: usize,

    ticked: Instant,
}

impl Typewriter {
    /// Initialize counts from the existing UTF-8 prefix once per reply.
    fn start(index: usize, text: &str, previous_bytes: usize, now: Instant) -> Self {
        let shown_bytes = text.floor_char_boundary(previous_bytes);
        let shown = text[..shown_bytes].chars().count();
        let total_chars = shown + text[shown_bytes..].chars().count();

        Self {
            index,
            shown: shown as f64,
            shown_bytes,
            total_chars,
            total_bytes: text.len(),
            #[cfg(test)]
            scanned_bytes: text.len(),
            ticked: now,
        }
    }

    fn update(&mut self, text: &str, change: EntryChange, now: Instant) {
        let was_moving = self.moving();

        if !was_moving {
            self.ticked = now;
        }

        match change {
            EntryChange::Append { previous_bytes }
                if previous_bytes == self.total_bytes && text.is_char_boundary(previous_bytes) =>
            {
                self.total_chars += text[previous_bytes..].chars().count();

                #[cfg(test)]
                {
                    self.scanned_bytes += text.len() - previous_bytes;
                }
            }
            _ => {
                self.total_chars = text.chars().count();

                self.shown = if was_moving {
                    self.shown.min(self.total_chars as f64)
                } else {
                    self.total_chars as f64
                };

                self.shown_bytes = shown_prefix(text, self.shown()).len();

                #[cfg(test)]
                {
                    self.scanned_bytes += text.len() + self.shown_bytes;
                }
            }
        }

        self.total_bytes = text.len();
    }

    fn moving(&self) -> bool {
        self.shown < self.total_chars as f64
    }

    fn index(&self) -> usize {
        self.index
    }

    /// Advance towards the stored character count and report pending text.
    ///
    /// The step is the larger of the catch-up share and the floor, so a big
    /// chunk closes quickly while a lone character still arrives promptly.
    /// A long gap between frames (a pane that was hidden) closes the whole
    /// backlog at once instead of typing text the reader was not watching.
    fn advance(&mut self, text: &str, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.ticked).as_secs_f64();
        let previous = self.shown();

        self.ticked = now;

        let total = self.total_chars as f64;
        let backlog = (total - self.shown).max(0.0);
        let share = 1.0 - (-elapsed / CATCH_UP.as_secs_f64()).exp();
        let step = (backlog * share).max(FLOOR_RATE * elapsed).min(backlog);

        self.shown = (self.shown + step).min(total);

        let added = shown_prefix(&text[self.shown_bytes..], self.shown() - previous).len();

        self.shown_bytes += added;

        #[cfg(test)]
        {
            self.scanned_bytes += added;
        }

        self.shown < total
    }

    /// How many characters of the reply are on screen.
    fn shown(&self) -> usize {
        self.shown as usize
    }
}

/// The part of `text` an edge `chars` characters in lets through. Counted in
/// characters, not bytes, so a CJK reply types at the same pace as a
/// latin one and the cut never falls inside a character.
fn shown_prefix(text: &str, chars: usize) -> &str {
    text.char_indices()
        .nth(chars)
        .map_or(text, |(end, _)| &text[..end])
}
