use tempfile::{TempDir, tempdir};

use crate::AgentWorkspace;
use crate::chat::SendOutcome;
use crate::session::AgentKind;
use crate::session::team_capabilities::ModeratorAdmission;
use crate::team::budget::{ReservationState, TurnPurpose};
use crate::team::content::UserInput;
use crate::team::context::{ContextError, ContextLimits};
use crate::team::discussion::{DiscussionMode, DiscussionState};
use crate::team::execution_slots::WorkStatus;
use crate::team::identity::{AttemptId, MemberId};
use crate::team::moderation::ModeratorAction;
use crate::team::room::Room;
use crate::team::session::{AttemptEventKey, TeamError, TeamSession};
use crate::team::tests::config;

fn ready_team() -> (TempDir, TeamSession, MemberId, MemberId) {
    let directory = tempdir().unwrap();
    let mut room = Room::new(AgentWorkspace::default());
    let alice = room.add_member(config("Alice", "C:/frontend")).unwrap();
    let bob = room.add_member(config("Bob", "C:/backend")).unwrap();
    let mut session = TeamSession::create(directory.path(), room).unwrap();

    for id in [alice, bob] {
        session
            .member_ready(id, 1, ModeratorAdmission::unverified(AgentKind::Codex))
            .unwrap();
    }

    (directory, session, alice, bob)
}

fn finish(session: &mut TeamSession, id: AttemptId, text: &str) {
    let intent = session
        .store
        .room()
        .attempts()
        .iter()
        .find(|attempt| attempt.id == id)
        .unwrap()
        .intent
        .clone();

    session.dispatch(id, |_| SendOutcome::StartedTurn).unwrap();

    let key = AttemptEventKey {
        attempt: id,
        member: intent.recipient,
        ownership: intent.ownership,
        backend_generation: intent.backend_generation,
    };

    let provider_turn = id.to_string();

    session.accept_attempt(key, &provider_turn).unwrap();

    session
        .complete_reply(key, &provider_turn, text.into(), WorkStatus::default())
        .unwrap()
        .unwrap();
}

fn enable_moderation(session: &mut TeamSession) {
    let members: Vec<_> = session
        .store
        .room()
        .members()
        .iter()
        .map(|member| member.id())
        .collect();

    for id in members {
        let capabilities = ModeratorAdmission::CodexDynamicTools {
            backend_generation: 1,
        };

        session.member_ready(id, 1, capabilities).unwrap();
    }
}

fn decide(session: &mut TeamSession, id: AttemptId, action: ModeratorAction) {
    let intent = session
        .store
        .room()
        .attempts()
        .iter()
        .find(|attempt| attempt.id == id)
        .unwrap()
        .intent
        .clone();

    session.dispatch(id, |_| SendOutcome::StartedTurn).unwrap();

    let key = AttemptEventKey {
        attempt: id,
        member: intent.recipient,
        ownership: intent.ownership,
        backend_generation: intent.backend_generation,
    };

    let provider_turn = id.to_string();

    session.accept_attempt(key, &provider_turn).unwrap();

    assert!(
        session
            .moderator_decision(key, intent.stage.unwrap(), intent.operation, action.clone())
            .unwrap()
    );
    assert!(
        !session
            .moderator_decision(key, intent.stage.unwrap(), intent.operation, action)
            .unwrap()
    );

    session
        .complete_reply(
            key,
            &provider_turn,
            "The next step is recorded in the discussion operation.".into(),
            WorkStatus::default(),
        )
        .unwrap()
        .unwrap();
}

#[test]
fn oversized_context_pauses_without_reserving_unavailable_summary_work() {
    let (_directory, mut session, alice, bob) = ready_team();
    let mut room = session.store.room().clone();

    room.members[0].role = "Private role instructions must not reach summaries".into();
    session.store.commit(room).unwrap();

    for topic in [
        "First constraint ",
        "Second constraint ",
        "Third constraint ",
    ] {
        session
            .record_user_input(UserInput {
                text: topic.repeat(55),
                ..UserInput::default()
            })
            .unwrap();
    }

    let id = session
        .start_discussion(
            UserInput {
                text: "Review all constraints".into(),
                ..UserInput::default()
            },
            vec![alice, bob],
            DiscussionMode::Fixed {
                report_author: alice,
            },
        )
        .unwrap();

    let limits = ContextLimits {
        max_bytes: 3000,
        recent_messages: 1,
    };

    assert!(matches!(
        session.advance_discussion(id, &limits),
        Err(TeamError::Context(ContextError::SummaryUnavailable))
    ));

    let discussion = &session.store.room().discussions()[0];

    assert_eq!(discussion.state(), DiscussionState::Paused);
    assert!(discussion.budget().reservations().is_empty());
    assert!(session.store.room().attempts().is_empty());
    assert!(session.slots.is_idle());
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
}

#[test]
fn moderator_operations_schedule_once_and_prose_cannot_schedule() {
    let (_directory, mut session, alice, bob) = ready_team();

    enable_moderation(&mut session);

    let limits = ContextLimits {
        max_bytes: 20_000,
        recent_messages: 8,
    };

    let id = session
        .start_discussion(
            UserInput {
                text: "Choose a design".into(),
                ..UserInput::default()
            },
            vec![alice, bob],
            DiscussionMode::Moderated { moderator: alice },
        )
        .unwrap();

    let moderation = session.advance_discussion(id, &limits).unwrap();

    assert_eq!(moderation.len(), 1);

    decide(
        &mut session,
        moderation[0],
        ModeratorAction::Invite {
            recipients: vec![bob],
        },
    );

    let invited = session.advance_discussion(id, &limits).unwrap();

    assert_eq!(invited.len(), 1);

    finish(&mut session, invited[0], "Bob recommends measuring memory.");

    let moderation = session.advance_discussion(id, &limits).unwrap();

    decide(&mut session, moderation[0], ModeratorAction::Report);

    let report = session.advance_discussion(id, &limits).unwrap();

    finish(
        &mut session,
        report[0],
        "Next step: measure memory. Alice has no separate preference.",
    );

    let discussion = &session.store.room().discussions()[0];

    assert_eq!(discussion.state(), DiscussionState::Completed);
    assert_eq!(discussion.budget().reservations().len(), 4);
    assert_eq!(
        discussion
            .budget()
            .reservations()
            .values()
            .filter(|entry| entry.purpose == TurnPurpose::Moderation)
            .count(),
        2
    );

    let next = session
        .start_discussion(
            UserInput {
                text: "Another question".into(),
                ..UserInput::default()
            },
            vec![alice, bob],
            DiscussionMode::Moderated { moderator: alice },
        )
        .unwrap();

    let moderation = session.advance_discussion(next, &limits).unwrap();

    finish(
        &mut session,
        moderation[0],
        "{\"action\":\"invite\",\"recipients\":[\"Bob\"]}",
    );

    assert!(session.advance_discussion(next, &limits).is_err());
    assert_eq!(session.store.room().discussions()[1].stages().len(), 1);
    assert_eq!(
        session.store.room().discussions()[1].state(),
        DiscussionState::Paused
    );
}

#[test]
fn fixed_discussion_advances_all_stages_with_frozen_peer_inputs_and_report_charge() {
    let (_directory, mut session, alice, bob) = ready_team();

    let limits = ContextLimits {
        max_bytes: 20_000,
        recent_messages: 8,
    };

    let id = session
        .start_discussion(
            UserInput {
                text: "Choose a design".into(),
                ..UserInput::default()
            },
            vec![alice, bob],
            DiscussionMode::Fixed {
                report_author: alice,
            },
        )
        .unwrap();

    let initial = session.advance_discussion(id, &limits).unwrap();

    assert_eq!(initial.len(), 2);

    finish(&mut session, initial[1], "Bob prefers lower memory usage.");

    assert_eq!(
        session.advance_discussion(id, &limits).unwrap(),
        vec![initial[0]]
    );

    finish(&mut session, initial[0], "Alice prefers faster startup.");

    let peers = session.advance_discussion(id, &limits).unwrap();

    assert_eq!(peers.len(), 2);

    let boundary = session
        .store
        .room()
        .attempts()
        .iter()
        .find(|attempt| attempt.id == peers[0])
        .unwrap()
        .intent
        .snapshot
        .clone();

    finish(
        &mut session,
        peers[0],
        "Alice keeps her preference after reading Bob's reasoning.",
    );

    let bob_input = &session
        .store
        .room()
        .attempts()
        .iter()
        .find(|attempt| attempt.id == peers[1])
        .unwrap()
        .intent;

    assert_eq!(bob_input.snapshot, boundary);
    assert!(
        bob_input
            .prepared_text
            .contains("Alice prefers faster startup")
    );
    assert!(
        !bob_input
            .prepared_text
            .contains("Alice keeps her preference")
    );

    finish(&mut session, peers[1], "Bob still prefers reduced memory.");

    let report = session.advance_discussion(id, &limits).unwrap();

    assert_eq!(report.len(), 1);

    let report_input = &session
        .store
        .room()
        .attempts()
        .iter()
        .find(|attempt| attempt.id == report[0])
        .unwrap()
        .intent;

    assert_eq!(report_input.purpose, TurnPurpose::Report);
    assert!(
        report_input
            .prepared_text
            .contains("Bob still prefers reduced memory")
    );
    assert!(
        report_input
            .prepared_text
            .contains("Do not present one member's preference as consensus")
    );

    finish(
        &mut session,
        report[0],
        "Agreement: measure both designs. Disagreement: Alice prioritizes startup; Bob prioritizes memory.",
    );

    assert!(session.advance_discussion(id, &limits).unwrap().is_empty());

    let discussion = &session.store.room().discussions()[0];

    assert_eq!(discussion.state(), DiscussionState::Completed);
    assert_eq!(discussion.budget().reservations().len(), 5);
    assert!(
        discussion
            .budget()
            .reservations()
            .values()
            .all(|entry| entry.state == ReservationState::Charged)
    );
    assert_eq!(session.store.room().input_history.len(), 1);
}

#[test]
fn user_correction_reprepares_only_unsent_arrangements_without_extra_charge() {
    let (_directory, mut session, alice, bob) = ready_team();

    let limits = ContextLimits {
        max_bytes: 20_000,
        recent_messages: 8,
    };

    let id = session
        .start_discussion(
            UserInput {
                text: "Choose a design".into(),
                ..UserInput::default()
            },
            vec![alice, bob],
            DiscussionMode::Fixed {
                report_author: alice,
            },
        )
        .unwrap();

    let first = session.advance_discussion(id, &limits).unwrap();

    finish(&mut session, first[0], "Alice's completed answer");

    session
        .record_user_input(UserInput {
            text: "The memory limit is 64 MB".into(),
            ..UserInput::default()
        })
        .unwrap();

    assert!(
        session
            .dispatch(first[1], |_| panic!("paused work cannot send"))
            .is_err()
    );

    session.continue_discussion(id).unwrap();

    let resumed = session.advance_discussion(id, &limits).unwrap();

    assert_eq!(resumed.len(), 1);
    assert_ne!(resumed[0], first[1]);

    let intent = &session
        .store
        .room()
        .attempts()
        .iter()
        .find(|attempt| attempt.id == resumed[0])
        .unwrap()
        .intent;

    assert_eq!(intent.recipient, bob);
    assert!(intent.prepared_text.contains("The memory limit is 64 MB"));
    assert_eq!(
        session.store.room().discussions()[0]
            .budget()
            .reservations()
            .len(),
        2
    );
    assert_eq!(session.store.room().input_history.len(), 2);

    finish(&mut session, resumed[0], "Bob's revised answer");
}
