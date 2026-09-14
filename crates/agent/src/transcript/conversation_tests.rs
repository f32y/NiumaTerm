use std::sync::Arc;

use crate::chat::{Item, ReplayTurn};
use crate::transcript::TextField;
use crate::transcript::conversation::{ConversationImage, ConversationState};

#[test]
fn indexed_updates_preserve_duplicate_kinds_and_missing_accounting() {
    let mut conversation = ConversationState::default();

    conversation.replay(
        1,
        ReplayTurn {
            items: Vec::new(),
            interrupted: false,
            seconds: None,
            output_tokens: None,
        },
    );

    assert!(conversation.turns.is_settled(1));
    assert_eq!(conversation.turns.seconds(1), None);
    assert_eq!(conversation.turns.output_tokens(1), None);

    conversation.push(
        2,
        Item::Reasoning {
            id: "shared".into(),
            summary: None,
        },
        Vec::new(),
    );

    conversation.push(
        2,
        Item::AgentMessage {
            id: "shared".into(),
            text: None,
            questions: None,
        },
        Vec::new(),
    );

    let version = conversation.version();

    let update = conversation
        .append_delta("shared", "answer", TextField::Reply)
        .unwrap();

    assert_eq!(update.index, 1);
    assert_eq!(conversation.content.latest_agent_message(2), Some("answer"));
    assert!(matches!(
        conversation.content.entries()[0].item,
        Item::Reasoning { summary: None, .. }
    ));
    assert_eq!(conversation.changes_since(version).unwrap().first, 1);
    assert_eq!(conversation.live.output_tokens(), None);
    assert!(conversation.context_window_usage.is_none());
}

#[test]
fn missed_updates_recover_and_clear_releases_accepted_resources() {
    let mut conversation = ConversationState::default();

    let image = Arc::new(ConversationImage::new(Arc::from([1, 2, 3])));
    let weak = Arc::downgrade(&image);

    conversation.push(
        1,
        Item::UserMessage {
            text: Some("image".into()),
        },
        vec![image],
    );

    conversation.push(
        1,
        Item::AgentMessage {
            id: "reply".into(),
            text: None,
            questions: None,
        },
        Vec::new(),
    );

    let version = conversation.version();

    for _ in 0..70 {
        conversation.append_delta("reply", "x", TextField::Reply);
    }

    assert_eq!(conversation.changes_since(version).unwrap().first, 0);
    assert_eq!(conversation.changes.len(), 64);
    assert_eq!(
        conversation.content.latest_agent_message(1).unwrap().len(),
        70
    );
    assert!(weak.upgrade().is_some());

    conversation.clear();

    assert!(weak.upgrade().is_none());
    assert_ne!(conversation.version().0, version.0);
    assert!(conversation.content.entries().is_empty());
}
