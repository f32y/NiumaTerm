use crate::AgentWorkspace;
use crate::chat::ThreadSettings;
use crate::session::AgentKind;
use crate::team::budget::{Budget, BudgetError, TurnPurpose};
use crate::team::discussion::{
    Arrangement, ArrangementState, DiscussionError, DiscussionMode, DiscussionState, PauseReason,
    PublicSnapshot, Stage, StageKind,
};
use crate::team::identity::{
    AttemptId, InteractionId, MessageId, OperationId, OwnershipGeneration, StageId,
};
use crate::team::member::{HistoryScope, MemberConfig, ProfileReference};
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
    assert_ne!(
        room.member(alice).unwrap().conversation(),
        original.conversation()
    );
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
