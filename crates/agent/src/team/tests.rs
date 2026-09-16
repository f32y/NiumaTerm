use std::collections::BTreeSet;

use crate::AgentWorkspace;
use crate::chat::ThreadSettings;
use crate::session::AgentKind;
use crate::team::budget::{Budget, BudgetError, TurnPurpose};
use crate::team::discussion::{
    Arrangement, ArrangementState, DiscussionError, DiscussionMode, DiscussionState, PauseReason,
    PublicSnapshot, Stage, StageKind,
};
use crate::team::member::{HistoryScope, MemberConfig, ProfileReference};
use crate::team::model::{
    AttemptId, Author, ContextError, ContextLimits, InteractionId, MessageId, OperationId,
    OwnershipGeneration, PublicMessage, Publication, SourceFragment, StageId, Summary, SummaryId,
    UserInput,
};
use crate::team::room::{MemberError, Room};

pub(super) fn config(name: &str, root: &str) -> MemberConfig {
    MemberConfig {
        name: name.into(),
        profile: ProfileReference {
            kind: AgentKind::Codex,
            name: "Shared profile".into(),
        },
        roots: AgentWorkspace::single(Some(root.into())),
        settings: ThreadSettings {
            model: Some("initial-model".into()),
            sandbox: Some("read-only".into()),
            ..ThreadSettings::default()
        },
        role: String::new(),
        history: HistoryScope::CompletedPublic,
    }
}

#[test]
fn shared_profile_members_keep_independent_conversations_settings_and_roots() {
    let mut room = Room::new(AgentWorkspace::single(Some("C:/room".into())));

    let alice = room.add_member(config("Alice", "C:/frontend")).unwrap();
    let bob = room.add_member(config("Bob", "C:/backend")).unwrap();
    let original = room.member(bob).unwrap().clone();

    let mut settings = room.member(alice).unwrap().settings().clone();

    settings.model = Some("another-model".into());
    settings.sandbox = Some("workspace-write".into());

    room.set_member_settings(alice, OwnershipGeneration::default(), settings)
        .unwrap();

    room.members[0].coverage.messages.insert(MessageId::new());

    assert_ne!(alice, bob);
    assert_eq!(room.member(bob).unwrap(), &original);
    assert_eq!(
        room.member(alice).unwrap().roots().primary(),
        Some("C:/frontend")
    );
    assert_eq!(
        room.member(bob).unwrap().roots().primary(),
        Some("C:/backend")
    );
    assert_eq!(room.workspace().primary(), Some("C:/room"));
}

#[test]
fn duplicate_names_and_stale_settings_leave_member_state_unchanged() {
    let mut room = Room::new(AgentWorkspace::default());

    let alice = room.add_member(config("Alice", "C:/a")).unwrap();
    let bob = room.add_member(config("Bob", "C:/b")).unwrap();
    let before = room.clone();

    assert_eq!(
        room.add_member(config(" alice ", "C:/c")),
        Err(MemberError::DuplicateName)
    );
    assert_eq!(
        room.rename_member(bob, "ALICE"),
        Err(MemberError::DuplicateName)
    );
    assert_eq!(
        room.rename_member(alice, "\n"),
        Err(MemberError::InvalidName)
    );
    assert_eq!(
        room.set_member_settings(
            alice,
            OwnershipGeneration::default().next().unwrap(),
            ThreadSettings::default()
        ),
        Err(MemberError::StaleOwner)
    );
    assert_eq!(room, before);

    room.rename_member(alice, " Alice ").unwrap();

    assert_eq!(room.member(alice).unwrap().name(), "Alice");
}

#[test]
fn mode_changes_preserve_checkpoints_budget_and_independent_pause_reasons() {
    let mut room = Room::new(AgentWorkspace::default());

    let alice = room.add_member(config("Alice", "C:/a")).unwrap();
    let bob = room.add_member(config("Bob", "C:/b")).unwrap();

    room.create_discussion(
        "Compare approaches".into(),
        vec![alice, bob],
        DiscussionMode::Fixed {
            report_author: alice,
        },
    )
    .unwrap();

    let run = &mut room.discussions[0];
    let attempt = AttemptId::new();

    run.budget
        .reserve(&[(attempt, TurnPurpose::Response)])
        .unwrap();

    run.budget.charge(attempt).unwrap();

    run.stages.push(Stage {
        decision: None,
        id: StageId::new(),
        kind: StageKind::InitialAnswers,
        arrangements: vec![
            Arrangement {
                operation: OperationId::new(),
                recipient: alice,
                state: ArrangementState::Completed(attempt),
            },
            Arrangement {
                operation: OperationId::new(),
                recipient: bob,
                state: ArrangementState::Pending,
            },
        ],
        segments: vec![PublicSnapshot::default()],
    });

    let stages = run.stages.clone();
    let budget = run.budget.clone();
    let question = PauseReason::Interaction(InteractionId::new());
    let update = PauseReason::Maintenance("codex-installation".into());

    run.pause(question.clone());

    run.pause(update.clone());

    run.change_mode(DiscussionMode::Moderated { moderator: bob })
        .unwrap();

    assert!(run.resolve_pause(&update));
    assert!(run.pauses().contains(&question));
    assert!(run.pauses().contains(&PauseReason::ModeChange));
    assert_eq!(run.state(), DiscussionState::Paused);
    assert_eq!(run.stages(), stages);
    assert_eq!(run.budget(), &budget);
    assert_eq!(run.participants(), &[alice, bob]);
    assert_eq!(run.mode(), DiscussionMode::Moderated { moderator: bob });
    assert!(matches!(
        room.create_discussion(
            "Another".into(),
            vec![alice],
            DiscussionMode::Fixed {
                report_author: alice
            }
        ),
        Err(DiscussionError::AlreadyActive)
    ));

    room.add_member(config("Charlie", "C:/c")).unwrap();

    assert_eq!(room.discussions()[0].participants(), &[alice, bob]);
}

#[test]
fn stage_reservations_are_atomic_and_keep_the_report_turn() {
    let mut budget = Budget::discussion();

    let initial: Vec<_> = (0..10)
        .map(|_| (AttemptId::new(), TurnPurpose::Response))
        .collect();

    budget.reserve(&initial).unwrap();

    let before = budget.clone();

    let group = [
        (AttemptId::new(), TurnPurpose::Summary),
        (AttemptId::new(), TurnPurpose::Moderation),
    ];

    assert_eq!(budget.reserve(&group), Err(BudgetError::InsufficientTurns));
    assert_eq!(budget, before);

    budget.reserve(&group[..1]).unwrap();

    assert_eq!(
        budget.reserve(&group[1..]),
        Err(BudgetError::InsufficientTurns)
    );

    let report = AttemptId::new();

    budget.reserve(&[(report, TurnPurpose::Report)]).unwrap();

    budget.charge(report).unwrap();

    assert_eq!(
        budget.cancel_unsent(report),
        Err(BudgetError::AlreadyDispatched)
    );

    budget.cancel_unsent(group[0].0).unwrap();

    assert_eq!(
        budget.reserve(&group[1..]),
        Err(BudgetError::InsufficientTurns)
    );

    budget.add_turns(1).unwrap();

    budget.reserve(&group[1..]).unwrap();

    let charged = budget.clone();

    assert_eq!(
        budget.reserve(&[group[1]]),
        Err(BudgetError::DuplicateAttempt)
    );
    assert_eq!(budget, charged);
}

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
