use tempfile::{TempDir, tempdir};

use crate::AgentWorkspace;
use crate::chat::SendOutcome;
use crate::session::AgentKind;
use crate::session::team_capabilities::ModeratorAdmission;
use crate::team::attempt::{AttemptState, BudgetScope};
use crate::team::budget::TurnPurpose;
use crate::team::discussion::{
    ArrangementState, DiscussionMode, DiscussionState, ModeratorAction, PauseReason, StageKind,
};
use crate::team::model::{
    AttemptId, Author, ContextError, ContextLimits, DiscussionId, MemberId, UserInput,
};
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
        backend_generation: intent.backend_generation,
    };

    let provider_turn = id.to_string();

    session.accept_attempt(key, &provider_turn).unwrap();

    session
        .complete_reply(key, &provider_turn, text.into())
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
    assert!(session.store.room().attempts().is_empty());
    assert!(!session.has_live_attempts());
    assert!(session.store.room().coverage(alice).messages.is_empty());
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
    assert_eq!(
        session
            .store
            .room()
            .budget_attempts(BudgetScope::Discussion(id))
            .count(),
        4
    );
    assert_eq!(
        session
            .store
            .room()
            .budget_attempts(BudgetScope::Discussion(id))
            .filter(|entry| entry.intent.purpose == TurnPurpose::Moderation)
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
    assert_eq!(
        session
            .store
            .room()
            .budget_attempts(BudgetScope::Discussion(id))
            .count(),
        5
    );
    assert!(
        session
            .store
            .room()
            .budget_attempts(BudgetScope::Discussion(id))
            .all(|entry| matches!(entry.state, AttemptState::Completed { .. }))
    );
    assert_eq!(user_inputs(&session), 1);
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
        session
            .store
            .room()
            .budget_attempts(BudgetScope::Discussion(id))
            .count(),
        2
    );
    assert_eq!(user_inputs(&session), 2);

    finish(&mut session, resumed[0], "Bob's revised answer");
}

#[test]
fn confirmed_nondelivery_refunds_a_turn_but_unknown_or_failed_delivery_keeps_it() {
    for outcome in [
        SendOutcome::NotReady,
        SendOutcome::StartedTurn,
        SendOutcome::Steered,
        SendOutcome::Rejected {
            message: "transport rejected".into(),
        },
    ] {
        let (_directory, mut session, alice, bob) = ready_team();

        let id = session
            .start_discussion(
                UserInput {
                    text: "Review".into(),
                    ..UserInput::default()
                },
                vec![alice, bob],
                DiscussionMode::Fixed {
                    report_author: alice,
                },
            )
            .unwrap();

        let ids = session
            .advance_discussion(
                id,
                &ContextLimits {
                    max_bytes: 20_000,
                    recent_messages: 8,
                },
            )
            .unwrap();

        let intent = session.store.room().attempts()[0].intent.clone();

        session.dispatch(ids[0], |_| outcome.clone()).unwrap();

        let expected = match outcome {
            SendOutcome::NotReady => {
                assert_eq!(
                    session.store.room().attempts()[0].state,
                    AttemptState::Rejected
                );

                10
            }
            SendOutcome::StartedTurn => {
                assert_eq!(
                    session.store.room().attempts()[0].state,
                    AttemptState::Sending
                );

                let key = AttemptEventKey {
                    attempt: ids[0],
                    member: alice,
                    backend_generation: intent.backend_generation,
                };

                session.fail_attempt(key, false).unwrap();

                assert_eq!(
                    session.store.room().attempts()[0].state,
                    AttemptState::Failed
                );

                9
            }
            SendOutcome::Steered | SendOutcome::Rejected { .. } => {
                assert_eq!(
                    session.store.room().attempts()[0].state,
                    AttemptState::Uncertain
                );

                9
            }
        };

        assert_eq!(
            session
                .store
                .room()
                .discussion(id)
                .unwrap()
                .remaining_non_report_turns(session.store.room().attempts()),
            expected
        );
        assert!(
            session
                .dispatch(ids[0], |_| panic!("completed sends cannot be repeated"))
                .is_err()
        );
    }
}

fn fixed_discussion(session: &mut TeamSession, alice: MemberId, bob: MemberId) -> DiscussionId {
    session
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
        .unwrap()
}

const ROOMY: ContextLimits = ContextLimits {
    max_bytes: 20_000,
    recent_messages: 8,
};

#[test]
fn continue_asks_a_moderator_again_after_an_undecided_stage() {
    let (_directory, mut session, alice, bob) = ready_team();

    enable_moderation(&mut session);

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

    let moderation = session.advance_discussion(id, &ROOMY).unwrap();

    finish(&mut session, moderation[0], "I think Bob should answer.");

    assert!(session.advance_discussion(id, &ROOMY).is_err());

    session.continue_discussion(id).unwrap();

    let again = session.advance_discussion(id, &ROOMY).unwrap();

    assert_eq!(again.len(), 1);
    assert_ne!(again[0], moderation[0]);

    let stages = session.store.room().discussions()[0].stages();

    assert_eq!(stages.len(), 2);
    assert_eq!(stages[1].kind, StageKind::ModeratorDecision);
}

#[test]
fn continue_retries_after_the_context_limit_paused_a_discussion() {
    let (_directory, mut session, alice, bob) = ready_team();

    let id = fixed_discussion(&mut session, alice, bob);

    let tight = ContextLimits {
        max_bytes: 10,
        recent_messages: 1,
    };

    assert!(session.advance_discussion(id, &tight).is_err());
    assert!(
        session.store.room().discussions()[0]
            .pauses()
            .contains(&PauseReason::ContextSelection)
    );

    session.continue_discussion(id).unwrap();

    assert_eq!(session.advance_discussion(id, &ROOMY).unwrap().len(), 2);
}

#[test]
fn finishing_clears_a_dispatch_pause_and_schedules_the_report() {
    let (_directory, mut session, alice, bob) = ready_team();

    let id = fixed_discussion(&mut session, alice, bob);

    session
        .pause_discussion(id, PauseReason::DispatchUnavailable)
        .unwrap();

    session.finish_with_report(id).unwrap();

    assert_eq!(
        session.store.room().discussions()[0].state(),
        DiscussionState::Finishing
    );
    assert_eq!(session.advance_discussion(id, &ROOMY).unwrap().len(), 1);
}

#[test]
fn an_excluded_participant_leaves_later_stages_without_pausing_them() {
    let (_directory, mut session, alice, bob) = ready_team();

    let id = fixed_discussion(&mut session, alice, bob);

    session.exclude_member(bob).unwrap();

    assert!(session.store.room().discussions()[0].pauses().is_empty());

    let initial = session.advance_discussion(id, &ROOMY).unwrap();

    assert_eq!(initial.len(), 1);
    assert_eq!(
        session
            .store
            .room()
            .attempts()
            .iter()
            .find(|attempt| attempt.id == initial[0])
            .unwrap()
            .intent
            .recipient,
        alice
    );
}

#[test]
fn an_attempt_whose_member_exited_can_be_abandoned() {
    let (_directory, mut session, alice, bob) = ready_team();

    let id = fixed_discussion(&mut session, alice, bob);

    let initial = session.advance_discussion(id, &ROOMY).unwrap();

    session
        .dispatch(initial[0], |_| SendOutcome::StartedTurn)
        .unwrap();

    let recipient = session
        .store
        .room()
        .attempts()
        .iter()
        .find(|attempt| attempt.id == initial[0])
        .unwrap()
        .intent
        .recipient;

    assert!(
        session
            .fail_attempt(
                AttemptEventKey {
                    attempt: initial[0],
                    member: recipient,
                    backend_generation: 1,
                },
                true,
            )
            .unwrap()
    );

    assert_eq!(
        session
            .pending_recovery()
            .map(|attempt| attempt.id)
            .collect::<Vec<_>>(),
        [initial[0]]
    );

    session.abandon_restored_attempt(initial[0]).unwrap();

    assert!(session.pending_recovery().next().is_none());
}

#[test]
fn work_sent_under_an_ended_generation_awaits_recovery() {
    let (_directory, mut session, alice, bob) = ready_team();

    let id = fixed_discussion(&mut session, alice, bob);

    let initial = session.advance_discussion(id, &ROOMY).unwrap();

    session
        .dispatch(initial[0], |_| SendOutcome::StartedTurn)
        .unwrap();

    let recipient = session
        .store
        .room()
        .attempts()
        .iter()
        .find(|attempt| attempt.id == initial[0])
        .unwrap()
        .intent
        .recipient;

    session
        .member_ready(
            recipient,
            2,
            ModeratorAdmission::unverified(AgentKind::Codex),
        )
        .unwrap();

    assert_eq!(
        session
            .pending_recovery()
            .map(|attempt| attempt.id)
            .collect::<Vec<_>>(),
        [initial[0]]
    );
}

#[test]
fn reopening_drops_interaction_pauses_of_ended_sessions() {
    let (directory, mut session, alice, bob) = ready_team();

    let id = fixed_discussion(&mut session, alice, bob);

    session
        .pause_discussion(id, PauseReason::Interaction(bob))
        .unwrap();

    let room = session.store.room().id();

    drop(session);

    let reopened = TeamSession::open(directory.path(), room).unwrap();

    let pauses = reopened.store.room().discussions()[0].pauses();

    assert!(!pauses.contains(&PauseReason::Interaction(bob)));
    assert!(pauses.contains(&PauseReason::Reopened));
}

fn user_inputs(session: &TeamSession) -> usize {
    session
        .store
        .room()
        .messages()
        .iter()
        .filter(|message| message.author == Author::User)
        .count()
}

/// A steered send whose delivery was unknown and is then accepted by the
/// provider is running again: its arrangement is active and the
/// uncertainty no longer holds the discussion.
#[test]
fn accepting_an_uncertain_attempt_makes_its_arrangement_active_again() {
    let (_directory, mut session, alice, bob) = ready_team();

    let id = fixed_discussion(&mut session, alice, bob);
    let ids = session.advance_discussion(id, &ROOMY).unwrap();
    let intent = session.store.room().attempts()[0].intent.clone();

    session.dispatch(ids[0], |_| SendOutcome::Steered).unwrap();

    let key = AttemptEventKey {
        attempt: ids[0],
        member: intent.recipient,
        backend_generation: intent.backend_generation,
    };

    assert!(session.accept_attempt(key, "turn-1").unwrap());

    let discussion = session.store.room().discussion(id).unwrap();

    assert!(
        discussion
            .stages()
            .iter()
            .flat_map(|stage| stage.arrangements.iter())
            .any(|entry| entry.state == ArrangementState::Active(ids[0]))
    );
    assert!(
        !discussion
            .pauses()
            .contains(&PauseReason::UncertainAttempt(ids[0]))
    );
}
