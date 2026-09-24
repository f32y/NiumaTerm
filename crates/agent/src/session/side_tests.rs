use crate::chat::Item;
use crate::session::side::SideQuestions;

fn items(side: &SideQuestions) -> Vec<Item> {
    side.conversation()
        .borrow()
        .content
        .entries()
        .iter()
        .map(|entry| entry.item.clone())
        .collect()
}

#[test]
fn exchanges_render_as_one_turn_per_question() {
    let mut side = SideQuestions::default();

    side.ask("first".into(), "nmt-1".into());

    assert!(side.conversation().borrow().live.is_working());
    assert!(side.settle("nmt-1", Ok("one".into())));
    assert!(!side.conversation().borrow().live.is_working());

    side.ask("second".into(), "nmt-2".into());

    assert!(side.settle("nmt-2", Err("timed out".into())));

    assert_eq!(
        items(&side),
        vec![
            Item::UserMessage {
                text: Some("first".into())
            },
            Item::AgentMessage {
                id: "nmt-1".into(),
                text: Some("one".into()),
                questions: None,
            },
            Item::UserMessage {
                text: Some("second".into())
            },
            Item::Error {
                text: "timed out".into()
            },
        ]
    );

    let turns: Vec<u64> = side
        .conversation()
        .borrow()
        .content
        .entries()
        .iter()
        .map(|entry| entry.turn)
        .collect();

    assert_eq!(turns, vec![1, 1, 2, 2]);
}

#[test]
fn history_carries_only_answered_exchanges() {
    let mut side = SideQuestions::default();

    side.ask("first".into(), "nmt-1".into());
    side.settle("nmt-1", Ok("one".into()));
    side.ask("second".into(), "nmt-2".into());
    side.settle("nmt-2", Err("cancelled".into()));
    side.ask("third".into(), "nmt-3".into());

    assert_eq!(side.history(), vec![("first", "one")]);
    assert_eq!(side.pending(), Some("nmt-3"));
}

#[test]
fn close_empties_the_shared_conversation_and_ignores_late_answers() {
    let mut side = SideQuestions::default();

    let shared = side.conversation().clone();

    side.ask("question".into(), "nmt-1".into());

    assert_eq!(side.close().as_deref(), Some("nmt-1"));
    assert!(!side.settle("nmt-1", Ok("late".into())));
    assert!(!side.is_open());
    assert!(shared.borrow().content.entries().is_empty());
    assert!(!shared.borrow().live.is_working());
}

#[test]
fn settled_request_is_not_settled_twice() {
    let mut side = SideQuestions::default();

    side.ask("question".into(), "nmt-1".into());

    assert!(side.settle("nmt-1", Ok("answer".into())));
    assert!(!side.settle("nmt-1", Err("cancelled".into())));
    assert_eq!(items(&side).len(), 2);
}
