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

/// The ramp front-loads the distance: the content is nearly all the way there
/// by the halfway point and spends the rest settling, which is what lets a
/// duration this long still read as an immediate answer to the click. Every
/// disclosure travels on it, so the same share is covered at the same share
/// of whatever time that disclosure is given.
#[test]
fn a_reveal_ramps_from_its_start_to_rest() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.open(RevealKey::Row(1), start);

    let opening = reveals.progress(RevealKey::Row(1), 0, start);
    let middle = reveals.progress(RevealKey::Row(1), 0, start + BLOCK_MOTION.duration / 2);
    let arrived = reveals.progress(RevealKey::Row(1), 0, start + BLOCK_MOTION.duration);

    assert!(opening < 0.05, "opens from nothing, got {opening}");
    assert!(
        (0.9..1.0).contains(&middle),
        "covers most of the distance early, got {middle}"
    );
    assert_eq!(arrived, 1.0);
    assert_eq!(
        middle,
        {
            let mut run = Reveals::default();
            run.open(RevealKey::Group(0), start);
            run.progress(RevealKey::Group(0), 0, start + RUN_MOTION.duration / 2)
        },
        "a run's row is the same share along at the same share of its own time"
    );
}

/// Shutting is the entrance run backwards, so everything reading progress —
/// the block's height, its fade, the chevron's angle — mirrors without
/// knowing which way the disclosure is going.
#[test]
fn shutting_runs_the_same_ramp_backwards() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.close(RevealKey::Row(1), start);

    let closing = reveals.progress(RevealKey::Row(1), 0, start);
    let middle = reveals.progress(RevealKey::Row(1), 0, start + BLOCK_MOTION.duration / 2);
    let gone = reveals.progress(RevealKey::Row(1), 0, start + BLOCK_MOTION.duration);

    assert_eq!(closing, 1.0, "leaves from where it was sitting");
    assert!(
        middle < 0.1,
        "most of the way gone by half time, got {middle}"
    );
    assert_eq!(gone, 0.0);
}

/// A second click part-way through an exit resumes from what is on screen.
/// Restarting from the far end instead would snap the block shut before
/// reopening it, which is the one thing an impatient reader must not see.
#[test]
fn reversing_resumes_from_what_is_on_screen() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.close(RevealKey::Row(1), start);

    let turn = start + BLOCK_MOTION.duration / 10;
    let leaving = reveals.progress(RevealKey::Row(1), 0, turn);
    reveals.open(RevealKey::Row(1), turn);
    let returning = reveals.progress(RevealKey::Row(1), 0, turn);

    assert!(
        (0.05..0.95).contains(&leaving),
        "the exit had actually moved, got {leaving}"
    );
    assert!(
        (leaving - returning).abs() < 0.01,
        "{leaving} vs {returning}"
    );
}

/// Later steps of a run start later, so the run reads as unrolling downwards
/// from the toggle rather than appearing all at once.
#[test]
fn later_rows_of_a_run_start_later() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.open(RevealKey::Group(0), start);

    let at =
        |ordinal| reveals.progress(RevealKey::Group(0), ordinal, start + RUN_MOTION.stagger * 2);

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
    reveals.open(RevealKey::Group(0), start);

    let at = |ordinal| {
        reveals.progress(
            RevealKey::Group(0),
            ordinal,
            start + RUN_MOTION.stagger * REVEAL_STAGGER_LIMIT as u32 + Duration::from_millis(1),
        )
    };

    assert_eq!(at(REVEAL_STAGGER_LIMIT), at(REVEAL_STAGGER_LIMIT + 40));
}

/// The transcript asks for frames while anything is still moving, and stops
/// once everything has, so an idle conversation leaves the frame pump parked.
#[test]
fn reveals_settle_after_their_window() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.open(RevealKey::Group(0), start);

    assert!(!reveals.settled(start));
    assert!(
        !reveals.settled(start + RUN_MOTION.duration),
        "the last staggered row is still going after the first has arrived"
    );
    assert!(reveals.settled(start + RUN_MOTION.window()));

    reveals.end(RevealKey::Group(0));
    assert!(reveals.settled(start));
}

/// Only a finished exit reports itself, and only an exit does: the content of
/// an open disclosure has to survive its own entrance.
#[test]
fn only_a_finished_exit_asks_to_be_taken_down() {
    let mut reveals = Reveals::default();
    let start = Instant::now();
    reveals.open(RevealKey::Row(1), start);
    reveals.close(RevealKey::Group(0), start);

    assert!(reveals.shut(start).is_empty());
    assert_eq!(
        reveals.shut(start + RUN_MOTION.window()),
        vec![RevealKey::Group(0)]
    );
}
