use std::time::Duration;

use crate::transcript::reveal::*;

/// A disclosure the reader never opened in this session — one restored with a
/// conversation, or one whose entrance finished long ago — renders at rest
/// rather than starting an entrance the moment it scrolls into view.
#[test]
fn an_untracked_disclosure_is_already_at_rest() {
    let reveals = Reveals::default();

    assert_eq!(reveals.progress(RevealKey::Row(3), 0, Instant::now()), 1.0);
}

#[test]
fn a_reveal_ramps_from_its_start_to_rest() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.begin(RevealKey::Row(1), start);

    let opening = reveals.progress(RevealKey::Row(1), 0, start);
    let middle = reveals.progress(
        RevealKey::Row(1),
        0,
        start + Duration::from_millis(REVEAL_DURATION_MS / 2),
    );
    let arrived = reveals.progress(
        RevealKey::Row(1),
        0,
        start + Duration::from_millis(REVEAL_DURATION_MS),
    );

    assert!(opening < 0.05, "opens from nothing, got {opening}");
    assert!(
        (0.2..0.8).contains(&middle),
        "eases through the middle, got {middle}"
    );
    assert_eq!(arrived, 1.0);
}

/// Later steps of a run start later, so the run reads as unrolling downwards
/// from the toggle rather than appearing all at once.
#[test]
fn later_rows_of_a_run_start_later() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.begin(RevealKey::Group(0), start);

    let at = |ordinal| {
        reveals.progress(
            RevealKey::Group(0),
            ordinal,
            start + Duration::from_millis(REVEAL_STAGGER_MS * 2),
        )
    };

    assert!(at(0) > at(1), "{} vs {}", at(0), at(1));
    assert!(at(1) > at(2), "{} vs {}", at(1), at(2));
    assert_eq!(at(2), 0.0, "a row whose turn has not come has not started");
}

/// The delay stops growing past the limit, so a long run finishes arriving in
/// a bounded time however many steps it holds.
#[test]
fn the_stagger_stops_growing_past_its_limit() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.begin(RevealKey::Group(0), start);

    let at = |ordinal| {
        reveals.progress(
            RevealKey::Group(0),
            ordinal,
            start + Duration::from_millis(REVEAL_STAGGER_MS * REVEAL_STAGGER_LIMIT as u64 + 1),
        )
    };

    assert_eq!(at(REVEAL_STAGGER_LIMIT), at(REVEAL_STAGGER_LIMIT + 40));
}

/// The transcript asks for frames while anything is still arriving, and stops
/// once everything has, so an idle conversation leaves the frame pump parked.
#[test]
fn reveals_settle_after_their_window() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.begin(RevealKey::Group(0), start);

    assert!(!reveals.settled(start));
    assert!(!reveals.settled(start + Duration::from_millis(REVEAL_DURATION_MS)));
    assert!(reveals.settled(start + REVEAL_WINDOW));

    reveals.end(RevealKey::Group(0));
    assert!(reveals.settled(start));
}
