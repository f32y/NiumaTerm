use std::time::{Duration, Instant};

use crate::transcript::typewriter::*;

const FRAME: Duration = Duration::from_millis(16);

/// A chunk the size of a sentence is most of the way on screen a quarter of
/// a second after it lands, which is the pace that reads as typing rather than
/// as text being held back from the reader.
#[test]
fn a_chunk_is_mostly_typed_within_a_quarter_second() {
    let start = Instant::now();
    let mut typewriter = Typewriter::start(0, 0, start);

    let mut now = start;
    while now < start + Duration::from_millis(250) {
        now += FRAME;
        typewriter.advance(40, now);
    }

    assert!(
        typewriter.shown() >= 34,
        "most of the chunk is through by then, got {}",
        typewriter.shown()
    );
}

/// The share alone would let one waiting character trickle in over many
/// frames; the floor brings it through within a frame or two.
#[test]
fn a_single_waiting_character_arrives_at_typing_pace() {
    let start = Instant::now();
    let mut typewriter = Typewriter::start(0, 10, start);

    let waiting = typewriter.advance(11, start + Duration::from_millis(20));

    assert_eq!(typewriter.shown(), 11);
    assert!(!waiting, "nothing is left behind the edge");
}

/// The edge stops at the text: a frame arriving long after the stream went
/// quiet lands everything and reports nothing left to type.
#[test]
fn the_edge_never_passes_the_text() {
    let start = Instant::now();
    let mut typewriter = Typewriter::start(0, 0, start);

    let waiting = typewriter.advance(5, start + Duration::from_secs(10));

    assert_eq!(typewriter.shown(), 5);
    assert!(!waiting);
}

/// The cut lands between characters, whatever their width in bytes, and a
/// count past the end lets the whole text through.
#[test]
fn a_prefix_ends_on_a_character_boundary() {
    assert_eq!(shown_prefix("你好世界", 2), "你好");
    assert_eq!(shown_prefix("ab", 5), "ab");
    assert_eq!(shown_prefix("ab", 0), "");
}
