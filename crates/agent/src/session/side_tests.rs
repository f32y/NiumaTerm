use crate::session::side::{SideAnswer, SideQuestions};

#[test]
fn history_carries_only_answered_exchanges_in_order() {
    let mut side = SideQuestions::default();

    side.ask("first".into(), "nmt-1".into());
    side.ask("second".into(), "nmt-2".into());
    side.ask("third".into(), "nmt-3".into());

    assert!(side.settle("nmt-1", Ok("one".into())));
    assert!(side.settle("nmt-2", Err("timed out".into())));

    assert_eq!(side.history(), vec![("first", "one")]);
    assert_eq!(side.pending(), Some("nmt-3"));
}

#[test]
fn answer_after_close_is_ignored() {
    let mut side = SideQuestions::default();

    side.ask("question".into(), "nmt-1".into());

    assert_eq!(side.close().as_deref(), Some("nmt-1"));
    assert!(!side.settle("nmt-1", Ok("late".into())));
    assert!(!side.is_open());
}

#[test]
fn settled_request_is_not_settled_twice() {
    let mut side = SideQuestions::default();

    side.ask("question".into(), "nmt-1".into());

    assert!(side.settle("nmt-1", Ok("answer".into())));
    assert!(!side.settle("nmt-1", Err("cancelled".into())));

    assert_eq!(
        side.exchanges()[0].answer,
        SideAnswer::Answered("answer".into())
    );
}
