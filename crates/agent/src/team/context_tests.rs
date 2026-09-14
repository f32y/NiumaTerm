use std::collections::BTreeSet;

use crate::AgentWorkspace;
use crate::team::content::{
    Author, PublicMessage, Publication, SourceFragment, Summary, UserInput,
};
use crate::team::context::{ContextError, ContextLimits};
use crate::team::identity::{MessageId, SummaryId};
use crate::team::member::HistoryScope;
use crate::team::room::Room;
use crate::team::tests::config;

fn message(text: &str) -> PublicMessage {
    PublicMessage {
        id: MessageId::new(),
        author: Author::User,
        publication: Publication::UserInput,
        text: text.into(),
        replies_to: Vec::new(),
        attachments: Vec::new(),
    }
}

#[test]
fn partial_summary_does_not_hide_an_uncovered_tail() {
    let mut room = Room::new(AgentWorkspace::default());
    let alice = room.add_member(config("Alice", "C:/a")).unwrap();
    let source = message("First half. Second half.");
    let source_id = source.id;
    let length = source.text.len();

    room.messages.push(source);

    let first = Summary {
        id: SummaryId::new(),
        version: 1,
        owner: alice,
        sources: vec![source_id],
        fragments: vec![SourceFragment {
            source: source_id,
            start: 0,
            end: 12,
        }],
        prior_summaries: Vec::new(),
        goals: "First half summary".into(),
        constraints: String::new(),
        agreements: String::new(),
        disagreements: Vec::new(),
    };

    room.summaries.push(first.clone());

    let limits = ContextLimits {
        max_bytes: 10_000,
        recent_messages: 0,
    };

    let partial = room
        .prepare_context(
            alice,
            &room.public_snapshot(),
            &UserInput::default(),
            &limits,
        )
        .unwrap();

    assert!(partial.text.contains("Second half."));
    assert!(partial.coverage.messages.contains(&source_id));

    room.summaries.push(Summary {
        id: SummaryId::new(),
        fragments: vec![SourceFragment {
            source: source_id,
            start: 12,
            end: length,
        }],
        goals: "Second half summary".into(),
        ..first
    });

    let complete = room
        .prepare_context(
            alice,
            &room.public_snapshot(),
            &UserInput::default(),
            &limits,
        )
        .unwrap();

    assert!(!complete.coverage.messages.contains(&source_id));
    assert_eq!(complete.coverage.summaries.len(), 2);
    assert!(complete.text.contains("First half summary"));
    assert!(complete.text.contains("Second half summary"));
}

#[test]
fn stage_snapshots_and_coverage_keep_late_replies_without_same_stage_leakage() {
    let mut room = Room::new(AgentWorkspace::default());
    let alice = room.add_member(config("Alice", "C:/a")).unwrap();
    let bob = room.add_member(config("Bob", "C:/b")).unwrap();
    let request = message("Original objective");

    room.messages.push(request.clone());

    let boundary = room.public_snapshot();

    room.messages.push(message("Alice completed later"));

    let late_id = room.messages[1].id;

    room.messages.push(message("Bob completed first"));

    let accepted_id = room.messages[2].id;

    room.members[1].coverage.messages.insert(accepted_id);

    let input = UserInput {
        text: "Respond to peers".into(),
        ..UserInput::default()
    };

    let limits = ContextLimits {
        max_bytes: 10_000,
        recent_messages: 8,
    };

    let same_stage = room
        .prepare_context(bob, &boundary, &input, &limits)
        .unwrap();

    assert!(same_stage.text.contains("Original objective"));
    assert!(!same_stage.text.contains("Alice completed later"));

    let next_stage = room
        .prepare_context(bob, &room.public_snapshot(), &input, &limits)
        .unwrap();

    assert!(next_stage.coverage.messages.contains(&late_id));
    assert!(!next_stage.coverage.messages.contains(&accepted_id));
    assert!(next_stage.text.contains("Alice completed later"));
    assert!(!next_stage.text.contains("Bob completed first"));
    assert!(room.member(alice).unwrap().coverage().messages.is_empty());
    assert_eq!(input.text, "Respond to peers");
}

#[test]
fn summary_scope_cannot_include_omitted_sources_and_disabling_requires_originals() {
    let mut room = Room::new(AgentWorkspace::default());
    let alice = room.add_member(config("Alice", "C:/a")).unwrap();
    let omitted = message("Omitted topic");
    let selected = message("Selected topic");

    room.messages.extend([omitted.clone(), selected.clone()]);

    let summary = Summary {
        fragments: Vec::new(),
        id: SummaryId::new(),
        version: 1,
        owner: alice,
        sources: vec![omitted.id, selected.id],
        prior_summaries: Vec::new(),
        goals: "Both topics".into(),
        constraints: String::new(),
        agreements: String::new(),
        disagreements: Vec::new(),
    };

    room.summaries.push(summary.clone());

    room.members[0].history = HistoryScope::Selected {
        messages: BTreeSet::from([selected.id]),
        summaries: BTreeSet::new(),
    };

    let limits = ContextLimits {
        max_bytes: 10_000,
        recent_messages: 0,
    };

    let prepared = room
        .prepare_context(
            alice,
            &room.public_snapshot(),
            &UserInput::default(),
            &limits,
        )
        .unwrap();

    assert!(prepared.text.contains("Selected topic"));
    assert!(!prepared.text.contains("Both topics"));
    assert!(!prepared.text.contains("Omitted topic"));

    room.members[0].history = HistoryScope::CompletedPublic;
    room.controls.automatic_summaries = false;

    let prepared = room
        .prepare_context(
            alice,
            &room.public_snapshot(),
            &UserInput::default(),
            &limits,
        )
        .unwrap();

    assert!(prepared.text.contains("Omitted topic"));
    assert!(!prepared.text.contains("Both topics"));
    assert_eq!(room.summaries[0], summary);
}

#[test]
fn oversized_public_context_is_rejected_without_truncating_sources() {
    let mut room = Room::new(AgentWorkspace::default());
    let alice = room.add_member(config("Alice", "C:/a")).unwrap();
    let original = message(&"Résumé 😀 ".repeat(1000));

    room.messages.push(original.clone());

    let limits = ContextLimits {
        max_bytes: 2048,
        recent_messages: 0,
    };

    let result = room.prepare_context(
        alice,
        &room.public_snapshot(),
        &UserInput::default(),
        &limits,
    );

    assert!(matches!(result, Err(ContextError::SummaryUnavailable)));
    assert_eq!(room.messages[0].text, original.text);

    room.controls.automatic_summaries = false;

    assert!(matches!(
        room.prepare_context(
            alice,
            &room.public_snapshot(),
            &UserInput::default(),
            &limits
        ),
        Err(ContextError::SelectRange)
    ));
}
