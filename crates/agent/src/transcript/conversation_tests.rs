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
            generation_samples: Vec::new(),
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

#[test]
fn generation_modes_share_samples_and_prefer_whole_log_totals_after_replay() {
    use crate::chat::{GenerationSample, SessionStats};
    use std::time::Duration;

    let mut conversation = ConversationState::default();

    for (turn, tokens, seconds) in [(1, 1000, 10), (2, 100, 2)] {
        conversation.replay(
            turn,
            ReplayTurn {
                generation_samples: vec![GenerationSample {
                    response_id: format!("{turn}:1"),
                    output_tokens: tokens,
                    elapsed: Duration::from_secs(seconds),
                    estimated: false,
                }],
                ..ReplayTurn::default()
            },
        );
    }

    assert_eq!(
        conversation
            .generation_stats
            .speed()
            .unwrap()
            .tokens_per_second,
        50.0
    );
    assert!(
        (conversation
            .session_generation_speed()
            .unwrap()
            .tokens_per_second
            - 1100.0 / 12.0)
            .abs()
            < 0.001
    );

    conversation.session_stats = Some(SessionStats {
        decode_tokens: 6000,
        decode_ms: 100_000,
        ..SessionStats::default()
    });

    assert_eq!(
        conversation
            .session_generation_speed()
            .unwrap()
            .tokens_per_second,
        60.0
    );
    assert_eq!(
        conversation
            .generation_stats
            .speed()
            .unwrap()
            .tokens_per_second,
        50.0
    );

    conversation.start();

    assert!(conversation.generation_stats.speed().is_none());
    assert_eq!(
        conversation
            .session_generation_speed()
            .unwrap()
            .tokens_per_second,
        60.0
    );

    conversation.session_stats = Some(SessionStats::default());

    assert!(conversation.session_generation_speed().is_none());

    conversation.clear();

    assert!(conversation.session_generation_speed().is_none());
}
