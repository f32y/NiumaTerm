use tempfile::tempdir;

use crate::AgentWorkspace;
use crate::chat::{SendOutcome, ThreadSettings};
use crate::session::AgentKind;
use crate::session::team_capabilities::ModeratorAdmission;
use crate::team::attempt::{AttemptState, BudgetScope, DispatchIntent};
use crate::team::budget::TurnPurpose;
use crate::team::content::{Author, PublicMessage, Publication, UserInput};
use crate::team::context::ContextLimits;
use crate::team::discussion::{DiscussionMode, DiscussionState, PublicSnapshot};
use crate::team::execution_slots::WorkStatus;
use crate::team::identity::{MessageId, OperationId};
use crate::team::room::Room;
use crate::team::session::{AttemptEventKey, TeamError, TeamSession};
use crate::team::tests::config;

#[test]
fn native_member_settings_allow_concurrent_work_without_room_permission_gates() {
    let directory = tempdir().unwrap();
    let mut room = Room::new(AgentWorkspace::default());

    let settings = ThreadSettings {
        sandbox: Some("workspace-write".into()),
        approval: Some("on-request".into()),
        ..ThreadSettings::default()
    };

    let [alice, bob, _] = ["Alice", "Bob", "Unstarted"].map(|name| {
        let mut member_config = config(name, "C:/workspace");

        member_config.settings = settings.clone();

        room.add_member(member_config).unwrap()
    });

    let mut session = TeamSession::create(directory.path(), room).unwrap();

    for id in [alice, bob] {
        session
            .member_ready(id, 1, ModeratorAdmission::unverified(AgentKind::Codex))
            .unwrap();
    }

    let discussion = session
        .start_discussion(
            UserInput {
                text: "Implement the change".into(),
                ..UserInput::default()
            },
            vec![alice, bob],
            DiscussionMode::Fixed {
                report_author: alice,
            },
        )
        .unwrap();

    let attempts = session
        .advance_discussion(
            discussion,
            &ContextLimits {
                max_bytes: 10_000,
                recent_messages: 8,
            },
        )
        .unwrap();

    assert_eq!(attempts.len(), 2);

    for attempt in attempts {
        assert_eq!(
            session
                .dispatch(attempt, |_| SendOutcome::StartedTurn)
                .unwrap(),
            SendOutcome::StartedTurn
        );
    }

    let ownership = session.store.room().member(alice).unwrap().ownership();

    let updated = ThreadSettings {
        sandbox: Some("read-only".into()),
        approval: Some("never".into()),
        ..ThreadSettings::default()
    };

    session
        .set_member_settings(alice, ownership, updated.clone())
        .unwrap();

    assert_eq!(
        session.store.room().member(alice).unwrap().settings(),
        &updated
    );
    assert_eq!(
        session.store.room().member(bob).unwrap().settings(),
        &settings
    );
    assert!(session.validate_member(alice).is_ok());
    assert!(session.validate_member(bob).is_ok());

    let run = &session.store.room().discussions()[0];

    assert!(run.pauses().is_empty());
    assert_eq!(run.state(), DiscussionState::Running);
}

#[test]
fn live_dispatch_requires_a_ready_member_and_reopen_rejects_uncertain_retry() {
    let directory = tempdir().unwrap();
    let mut room = Room::new(AgentWorkspace::default());
    let alice = room.add_member(config("Alice", "C:/frontend")).unwrap();
    let member = room.member(alice).unwrap();
    let room_id = room.id();
    let operation = OperationId::new();

    let intent = DispatchIntent {
        invocation: Default::default(),
        attachments: Vec::new(),
        recipient: alice,
        ownership: member.ownership(),
        backend_generation: 1,
        operation,
        stage: None,
        budget: BudgetScope::Direct(operation),
        purpose: TurnPurpose::Response,
        input: UserInput {
            text: "Inspect the sources".into(),
            ..UserInput::default()
        },
        prepared_text: "Inspect the sources".into(),
        snapshot: PublicSnapshot::default(),
        coverage: Default::default(),
    };

    let mut session = TeamSession::create(directory.path(), room).unwrap();

    assert!(matches!(
        session.reserve_dispatches(vec![intent.clone()]),
        Err(TeamError::Unavailable)
    ));

    session
        .member_ready(alice, 1, ModeratorAdmission::unverified(AgentKind::Codex))
        .unwrap();

    let id = session.reserve_dispatches(vec![intent.clone()]).unwrap()[0];
    let mut sends = 0;

    session
        .dispatch(id, |_| {
            sends += 1;

            SendOutcome::StartedTurn
        })
        .unwrap();

    assert!(
        session
            .dispatch(id, |_| {
                sends += 1;
                SendOutcome::StartedTurn
            })
            .is_err()
    );
    assert_eq!(sends, 1);

    drop(session);

    let (mut session, _) = TeamSession::open(directory.path(), room_id).unwrap();

    session
        .member_ready(alice, 2, ModeratorAdmission::unverified(AgentKind::Codex))
        .unwrap();

    assert!(
        session
            .dispatch(id, |_| {
                sends += 1;
                SendOutcome::StartedTurn
            })
            .is_err()
    );
    assert_eq!(sends, 1);
    assert_eq!(
        session.store.room().attempts()[0].intent.input,
        intent.input
    );

    session.abandon_restored_attempt(id).unwrap();

    assert_eq!(
        session.store.room().attempts()[0].state,
        AttemptState::Abandoned
    );

    drop(session);

    let (session, _) = TeamSession::open(directory.path(), room_id).unwrap();

    assert!(session.pending_recovery().next().is_none());
    assert_eq!(
        session.store.room().attempts()[0].state,
        AttemptState::Abandoned
    );
    assert_eq!(
        session.store.room().attempts()[0].intent.input,
        intent.input
    );
}

#[test]
fn accepted_coverage_and_root_reply_commit_once_and_survive_reopening() {
    let directory = tempdir().unwrap();
    let mut room = Room::new(AgentWorkspace::default());
    let alice = room.add_member(config("Alice", "C:/frontend")).unwrap();
    let source = MessageId::new();

    room.messages.push(PublicMessage {
        id: source,
        author: Author::User,
        publication: Publication::UserInput,
        text: "Compare both options".into(),
        replies_to: Vec::new(),
        attachments: Vec::new(),
    });

    let room_id = room.id();
    let mut session = TeamSession::create(directory.path(), room).unwrap();

    session
        .member_ready(alice, 7, ModeratorAdmission::unverified(AgentKind::Codex))
        .unwrap();

    let operation = OperationId::new();
    let member = session.store.room().member(alice).unwrap();
    let ownership = member.ownership();
    let snapshot = session.store.room().public_snapshot();

    let input = UserInput {
        text: "Compare".into(),
        ..UserInput::default()
    };

    let limits = ContextLimits {
        max_bytes: 10_000,
        recent_messages: 8,
    };

    let prepared = session
        .store
        .room()
        .prepare_context(alice, &snapshot, &input, &limits)
        .unwrap();

    let intent = DispatchIntent {
        invocation: Default::default(),
        attachments: prepared.attachments,
        recipient: alice,
        ownership,
        backend_generation: 7,
        operation,
        stage: None,
        budget: BudgetScope::Direct(operation),
        purpose: TurnPurpose::Response,
        input,
        prepared_text: prepared.text,
        snapshot,
        coverage: prepared.coverage,
    };

    let id = session.reserve_dispatches(vec![intent]).unwrap()[0];

    session.dispatch(id, |_| SendOutcome::StartedTurn).unwrap();

    assert!(
        session
            .store
            .room()
            .member(alice)
            .unwrap()
            .coverage()
            .messages
            .is_empty()
    );

    let key = AttemptEventKey {
        attempt: id,
        member: alice,
        ownership,
        backend_generation: 7,
    };

    assert!(
        !session
            .accept_attempt(
                AttemptEventKey {
                    backend_generation: 6,
                    ..key
                },
                "turn-1"
            )
            .unwrap()
    );
    assert!(session.accept_attempt(key, "turn-1").unwrap());
    assert!(!session.accept_attempt(key, "turn-1").unwrap());
    assert!(
        session
            .store
            .room()
            .member(alice)
            .unwrap()
            .coverage()
            .messages
            .contains(&source)
    );
    assert!(
        session
            .complete_reply(
                key,
                "another-turn",
                "Wrong reply".into(),
                WorkStatus::default()
            )
            .unwrap()
            .is_none()
    );

    let reply = session
        .complete_reply(
            key,
            "turn-1",
            "Option A is faster; option B uses less memory.".into(),
            WorkStatus::default(),
        )
        .unwrap()
        .unwrap();

    assert!(
        session
            .complete_reply(key, "turn-1", "Duplicate".into(), WorkStatus::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(session.store.room().messages().len(), 2);
    assert!(
        session
            .store
            .room()
            .member(alice)
            .unwrap()
            .coverage()
            .messages
            .contains(&reply)
    );

    drop(session);

    let (session, _) = TeamSession::open(directory.path(), room_id).unwrap();

    assert_eq!(
        session.store.room().attempts()[0].provider_turn.as_deref(),
        Some("turn-1")
    );

    let prepared = session
        .store
        .room()
        .prepare_context(
            alice,
            &session.store.room().public_snapshot(),
            &UserInput::default(),
            &limits,
        )
        .unwrap();

    assert!(prepared.coverage.messages.is_empty());
    assert!(!prepared.text.contains("Option A is faster"));
}
