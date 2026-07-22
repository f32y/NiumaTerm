//! Visible-tier regex search over the render buffer. Flattens the visible grid
//! to text plus an exact char→`Pos` index —
//! soft-wrapped rows are joined (no separator at a wrap continuation), spacer
//! cells are skipped — then runs a regex over the text. Matches come back in
//! visible-row coordinates; the frontend lifts them to SCREEN via `viewport_top`.
//!
//! This is the common, cheap tier: O(viewport), no engine/formatter call. The
//! deep scrollback tier is separate. Because wrapped rows are joined, a pattern
//! spanning a soft-wrap boundary matches naturally; hard line
//! ends carry a `\n`, so `.` does not cross them.

use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::{Regex, RegexBuilder, escape};

use crate::terminal::grid::row::Row;
use crate::terminal::pos::{Column, Direction, Line, Pos};
use crate::terminal::square::{Square, Wide};

pub type Match = RangeInclusive<Pos>;

/// Global search-semantics toggle. When `true` (default), the search bar input is
/// matched **literally** — regex metacharacters are escaped, so `.` matches a
/// literal dot and `C:\` matches as typed (the common Ctrl+F expectation). Flip
/// to `false` for full **regex** search (`.` = any char, `\d+`, …). Read by
/// [`compile`]; flip via `LITERAL_SEARCH.store(false, Ordering::Relaxed)`.
pub static LITERAL_SEARCH: AtomicBool = AtomicBool::new(true);

/// Compile a search pattern honoring the global [`LITERAL_SEARCH`] toggle.
pub fn compile(pattern: &str) -> Option<Regex> {
    compile_with(pattern, LITERAL_SEARCH.load(Ordering::Relaxed))
}

/// Compile a pattern with smart case (case-insensitive unless it contains an
/// uppercase letter). `literal` escapes regex metacharacters first.
pub fn compile_with(pattern: &str, literal: bool) -> Option<Regex> {
    let has_uppercase = pattern.chars().any(|c| c.is_uppercase());
    let escaped = if literal { Some(escape(pattern)) } else { None };
    let pat = escaped.as_deref().unwrap_or(pattern);
    RegexBuilder::new(pat)
        .case_insensitive(!has_uppercase)
        .build()
        .ok()
}

/// A flattened, position-indexed copy of the visible render-buffer rows.
pub struct VisibleCorpus {
    text: String,
    /// Byte offset (start) of each emitted char in `text`, ascending; parallel
    /// to `pos`. `\n` separators are in `text` but have no entry here.
    byte: Vec<usize>,
    /// Visible-grid `Pos` of each emitted char, parallel to `byte`.
    pos: Vec<Pos>,
}

impl VisibleCorpus {
    /// Flatten the visible grid. `wrapped[y]` = row `y` soft-wraps into `y + 1`.
    pub fn build(rows: &[Row<Square>], cols: usize, wrapped: &[bool]) -> Self {
        let mut text = String::new();
        let mut byte = Vec::new();
        let mut pos = Vec::new();
        // Byte offset where the current logical line began, so trailing blanks
        // are trimmed only back to the line start (never into the previous line).
        let mut line_start = 0usize;

        for (y, row) in rows.iter().enumerate() {
            for x in 0..cols {
                let sq = row.inner.get(x).copied().unwrap_or_default();
                // Skip the trailing cells of a wide char; the base cell carries
                // the character.
                if matches!(sq.wide(), Wide::Spacer | Wide::LeadingSpacer) {
                    continue;
                }
                byte.push(text.len());
                pos.push(Pos::new(Line(y as i32), Column(x)));
                // Normalize never-written cells (`\0`) to space so trailing
                // padding trims uniformly and `.` treats them as blanks.
                let c = sq.c();
                text.push(if c == '\0' { ' ' } else { c });
            }
            // Join soft-wrapped rows; separate hard line ends with a newline so
            // `.` cannot cross them.
            if !wrapped.get(y).copied().unwrap_or(false) {
                // Drop trailing blank cells of the logical line so `.`/`$` don't
                // match the empty padding (each popped char is a 1-byte space).
                while text.len() > line_start && text.ends_with(' ') {
                    text.pop();
                    byte.pop();
                    pos.pop();
                }
                text.push('\n');
                line_start = text.len();
            }
        }

        Self { text, byte, pos }
    }

    /// Map a regex byte range to an inclusive visible-coord cell range.
    fn range(&self, start: usize, end: usize) -> Match {
        let last = self.pos.len() - 1;
        let s = self.byte.partition_point(|&b| b < start).min(last);
        let e = self
            .byte
            .partition_point(|&b| b < end)
            .saturating_sub(1)
            .min(last);
        self.pos[s]..=self.pos[e]
    }

    /// All matches, in ascending (row-major) order. Visible-coord, inclusive.
    pub fn find_all(&self, re: &Regex) -> Vec<Match> {
        if self.pos.is_empty() {
            return Vec::new();
        }
        re.find_iter(&self.text)
            .filter(|m| m.start() != m.end()) // ignore empty matches
            .map(|m| self.range(m.start(), m.end()))
            .collect()
    }

    /// The nearest match from `origin` in `direction`, wrapping. The match
    /// `start` is the anchor compared against `origin`. Visible-coord.
    pub fn find(&self, re: &Regex, origin: Pos, direction: Direction) -> Option<Match> {
        nearest_wrapping(&self.find_all(re), origin, direction, |m| *m.start()).cloned()
    }
}

/// Nearest element to `origin` in `direction` over an ascending list, wrapping
/// to the far end when nothing lies in that direction (`None` only when the
/// list is empty). `key` maps an element to the anchor compared with `origin`.
fn nearest_wrapping<T, K: PartialOrd>(
    items: &[T],
    origin: K,
    direction: Direction,
    key: impl Fn(&T) -> K,
) -> Option<&T> {
    match direction {
        Direction::Right => items
            .iter()
            .find(|item| key(item) >= origin)
            .or_else(|| items.first()),
        Direction::Left => items
            .iter()
            .rev()
            .find(|item| key(item) <= origin)
            .or_else(|| items.last()),
    }
}

/// Deep scrollback search corpus. Built from two
/// formatter PLAIN passes (both `trim=false`): `wrapped` (`unwrap=true`, for
/// matching — soft-wrapped rows joined so a pattern spanning a wrap is found) and
/// `unwrapped` (`unwrap=false`, one text line per grid row). A single co-walk maps
/// each `wrapped` byte offset → **grid row**. The deep tier resolves only the
/// match's row; the post-scroll visible re-scan gives the exact columns. Built
/// once per search session, invalidated on content-version bump, dropped on close.
pub struct DeepCorpus {
    /// `unwrap=true` text — the matching surface.
    text: String,
    /// `row_start[r]` = byte offset in `text` where grid row `r` begins. The two
    /// passes share identical non-`\n` bytes in order; `unwrapped` only inserts a
    /// `\n` at each soft-wrap continuation, which is where a new grid row starts
    /// without a separator in `text`.
    row_start: Vec<usize>,
}

impl DeepCorpus {
    /// Co-walk the two passes to build the offset→grid-row map. Both must be
    /// produced with `trim=false` (else trailing-blank trimming desyncs them).
    pub fn build(wrapped: String, unwrapped: &str) -> Self {
        let mut row_start = vec![0usize];
        let wb = wrapped.as_bytes();
        let ub = unwrapped.as_bytes();
        let mut wi = 0;
        let mut ui = 0;
        while ui < ub.len() {
            if ub[ui] == b'\n' {
                // End of a grid row in the unwrapped pass.
                ui += 1;
                if wi < wb.len() && wb[wi] == b'\n' {
                    // Hard line end — the wrapped pass has the `\n` too.
                    wi += 1;
                }
                // else soft-wrap continuation — the wrapped pass joined, no `\n`.
                row_start.push(wi);
            } else {
                // Identical (non-newline) byte in both passes.
                wi += 1;
                ui += 1;
            }
        }
        Self {
            text: wrapped,
            row_start,
        }
    }

    /// Total number of (non-empty) matches of `re` across the whole buffer. Used to
    /// show the match count for deep (scrollback-wide) search.
    pub fn count(&self, re: &Regex) -> usize {
        re.find_iter(&self.text)
            .filter(|m| m.start() != m.end())
            .count()
    }

    /// Grid row containing the byte `offset` in `text`.
    fn grid_row_of(&self, offset: usize) -> usize {
        self.row_start
            .partition_point(|&s| s <= offset)
            .saturating_sub(1)
    }

    /// The grid row of the nearest match to `origin_row` in `direction`, wrapping.
    /// `None` if the pattern matches nowhere. The caller scrolls that row into view
    /// and re-scans the visible buffer for the exact range.
    pub fn find_row(&self, re: &Regex, origin_row: i32, direction: Direction) -> Option<usize> {
        let rows: Vec<usize> = re
            .find_iter(&self.text)
            .filter(|m| m.start() != m.end())
            .map(|m| self.grid_row_of(m.start()))
            .collect();
        nearest_wrapping(&rows, origin_row, direction, |&r| r as i32).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::DeepCorpus;

    #[test]
    fn deep_corpus_maps_offset_to_grid_row() {
        let unwrapped = "foo\nbar\nbaz\nqux\n";
        let wrapped = "foo\nbarbaz\nqux\n".to_string();
        let corpus = DeepCorpus::build(wrapped, unwrapped);
        assert_eq!(corpus.grid_row_of(0), 0, "foo");
        assert_eq!(corpus.grid_row_of(4), 1, "wrap start");
        assert_eq!(corpus.grid_row_of(7), 2, "wrap continuation");
        assert_eq!(corpus.grid_row_of(11), 3, "qux");
    }
}
