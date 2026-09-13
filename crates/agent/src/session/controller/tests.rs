use std::iter;
use std::path::Path;
use std::time::Instant;

use crate::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskSnapshot,
};
use crate::chat::{
    Event, Item, ModelInfo, Question, QuestionInput, QuestionMode, QuestionRequest,
    QuestionResolution, SendOutcome, SlashCommandOutcome, ThreadSettings,
};
use crate::session::controller::{QuestionSubmission, SessionController, SessionEffect};
use crate::session::delivery::{RecoverablePrompt, Submission};
use crate::session::input::{ApprovalOutcome, QuestionAction, QuestionKey};
use crate::session::lifecycle::{InterruptOutcome, StartOutcome, Status};
use crate::session::restore::SettingsSeed;
use crate::session::test_support::TestBackend;
use crate::session::{AgentKind, Backend};

fn started(kind: AgentKind, id: &str, outcomes: Vec<SendOutcome>) -> SessionController {
    let mut session = SessionController::new(kind);
    let epoch = session.starting(None).epoch;

    let mut backend = TestBackend::new(outcomes, SlashCommandOutcome::NotReady, Vec::new())
        .with_recovery(kind, id);

    backend.interrupt_accepted = true;
    backend.approval_accepted = true;

    assert!(matches!(
        session.install(epoch, Ok(Backend::Test(backend))),
        StartOutcome::Installed
    ));

    let SessionEffect::Ready(_) =
        session.apply_event(epoch, Event::Ready(ThreadSettings::default()))
    else {
        panic!("startup must expose effective settings");
    };

    session
}

fn send(session: &mut SessionController, text: &str) -> Submission {
    session
        .submit(
            text.into(),
            |backend, text| {
                backend.send_user_message(
                    text,
                    &ThreadSettings::default(),
                    None,
                    iter::empty(),
                    Path::new("unused-test-attachments"),
                )
            },
            || {
                Some(RecoverablePrompt {
                    text: text.into(),
                    response_annotations: vec!["Selected response".into()],
                    skill: None,
                })
            },
        )
        .expect("no interaction blocks this submission")
}

fn apply(session: &mut SessionController, event: Event) -> SessionEffect {
    session.apply_event(session.runtime.epoch(), event)
}

fn request(session: &mut SessionController, id: &str) -> QuestionKey {
    let effect = apply(
        session,
        Event::InputRequested(QuestionRequest {
            id: id.into(),
            mode: QuestionMode::Async,
            questions: vec![Question {
                input: QuestionInput::Text,
                header: None,
                question: "Choose the next action".into(),
                multi_select: false,
                options: Vec::new(),
            }],
        }),
    );

    let SessionEffect::InputRequested { index } = effect else {
        panic!("new question must be admitted");
    };

    session.input.batches()[index].key()
}

#[test]
fn startup_rejects_sends_and_superseded_installation_cannot_replace_current_backend() {
    let mut session = SessionController::new(AgentKind::Codex);
    let old = session.starting(None).epoch;

    assert_eq!(send(&mut session, "draft"), Submission::NotReady);
    assert_eq!(session.delivery.turn(), 0);

    let current = session.starting(None).epoch;

    assert!(matches!(
        session.install(
            old,
            Ok(Backend::Test(TestBackend::new(
                [],
                SlashCommandOutcome::NotReady,
                Vec::new()
            )))
        ),
        StartOutcome::Superseded(Some(_))
    ));
    assert!(session.runtime.backend().is_none());
    assert!(matches!(
        session.install(current, Err("launcher unavailable".into())),
        StartOutcome::Failed(_)
    ));
    assert_eq!(session.runtime.status(), Status::Exited);
    assert!(!session.delivery.is_active());
}

#[test]
fn rejected_send_preserves_accepted_prompt_and_interrupt_is_consumed_once() {
    let mut session = started(
        AgentKind::Codex,
        "first",
        vec![
            SendOutcome::StartedTurn,
            SendOutcome::Rejected {
                message: "busy".into(),
            },
        ],
    );

    assert_eq!(
        send(&mut session, "accepted"),
        Submission::Started {
            text: "accepted".into()
        }
    );
    assert!(matches!(
        apply(&mut session, Event::TurnStarted),
        SessionEffect::TurnStarted { opened: false }
    ));
    assert_eq!(
        send(&mut session, "rejected"),
        Submission::Rejected {
            message: "busy".into()
        }
    );
    assert_eq!(session.delivery.turn(), 1);

    let stopped = session.interrupt_from_user();

    let (turn, prompt) = stopped
        .prompt
        .expect("unanswered prompt remains recoverable");

    assert_eq!(turn, 1);
    assert_eq!(prompt.text, "accepted");
    assert_eq!(prompt.response_annotations, ["Selected response"]);
    assert_eq!(stopped.outcome, InterruptOutcome::Accepted);
    assert!(matches!(
        apply(&mut session, Event::TurnCompleted { error: None }),
        SessionEffect::TurnCompleted {
            interrupted: true,
            ..
        }
    ));
    assert!(matches!(
        apply(&mut session, Event::TurnCompleted { error: None }),
        SessionEffect::TurnCompleted {
            interrupted: false,
            ..
        }
    ));
    assert_eq!(session.runtime.status(), Status::Idle);
}

#[test]
fn replacement_rejects_old_completion_settings_and_input_events() {
    let mut session = started(AgentKind::Codex, "first", vec![SendOutcome::StartedTurn]);
    let old = session.runtime.epoch();

    send(&mut session, "accepted");
    apply(&mut session, Event::TurnStarted);
    session.starting(None);

    for event in [
        Event::TurnCompleted { error: None },
        Event::Ready(ThreadSettings {
            model: Some("old-model".into()),
            ..Default::default()
        }),
        Event::ApprovalRequested {
            description: "old approval".into(),
        },
        Event::InputResolved {
            id: "old-question".into(),
            resolution: QuestionResolution::Skipped,
        },
    ] {
        assert!(matches!(
            session.apply_event(old, event),
            SessionEffect::Unchanged
        ));
    }

    assert_eq!(session.runtime.status(), Status::Starting);
    assert!(session.controls.settings.model.is_none());
    assert!(session.input.approval().is_none());
}

#[test]
fn provider_busy_input_keeps_its_existing_delivery_boundary() {
    for kind in [AgentKind::Codex, AgentKind::Claude, AgentKind::DeepSeek] {
        let mut session = started(
            kind,
            "thread",
            vec![SendOutcome::StartedTurn, SendOutcome::Steered],
        );

        send(&mut session, "first");
        apply(&mut session, Event::TurnStarted);

        assert_eq!(send(&mut session, "follow-up"), Submission::Queued);
        assert!(session.delivery.pop_confirmed().is_none());

        apply(&mut session, Event::TurnCompleted { error: None });

        let count = |session: &SessionController| {
            session.conversation.borrow().content.entries().iter().filter(|entry| matches!(&entry.item, Item::UserMessage { text: Some(text) } if text == "follow-up")).count()
        };

        assert_eq!(count(&session), usize::from(kind == AgentKind::Codex));

        apply(&mut session, Event::TurnStarted);

        assert_eq!(count(&session), usize::from(kind != AgentKind::DeepSeek));
        assert!(session.delivery.pop_confirmed().is_none());
    }
}

#[test]
fn independent_sessions_route_approval_and_question_answers_to_their_own_backend() {
    let mut alice = started(AgentKind::Codex, "alice", Vec::new());
    let mut bob = started(AgentKind::Codex, "bob", Vec::new());

    apply(
        &mut alice,
        Event::ApprovalRequested {
            description: "Read a directory".into(),
        },
    );

    let key = request(&mut bob, "choose");

    assert!(matches!(
        bob.submit_question(key, QuestionAction::Skip, Instant::now()),
        QuestionSubmission::Waiting
    ));
    assert!(alice.input.approval().is_some());
    assert_eq!(alice.respond_approval("accept"), ApprovalOutcome::Settled);

    let Some(Backend::Test(alice_backend)) = alice.runtime.backend() else {
        panic!("test backend");
    };

    let Some(Backend::Test(bob_backend)) = bob.runtime.backend() else {
        panic!("test backend");
    };

    assert_eq!(alice_backend.approval_responses, ["accept"]);
    assert!(alice_backend.input_responses.is_empty());
    assert!(bob_backend.approval_responses.is_empty());
    assert_eq!(bob_backend.input_responses.len(), 1);
    assert_eq!(bob_backend.input_responses[0].id, "choose");

    apply(
        &mut bob,
        Event::InputResolved {
            id: "choose".into(),
            resolution: QuestionResolution::Skipped,
        },
    );

    assert!(!bob.input.has_submission());
    assert!(matches!(
        bob.submit_question(key, QuestionAction::Skip, Instant::now()),
        QuestionSubmission::Ignored
    ));
}

#[test]
fn child_snapshots_are_visible_only_to_the_matching_parent_and_epoch() {
    let mut session = started(AgentKind::Codex, "current", Vec::new());
    let epoch = session.runtime.epoch();

    let snapshot = |id| BackgroundTaskSnapshot {
        parent_session: BackgroundTaskKey::codex(id),
        tasks: Vec::new(),
        discovery: BackgroundTaskDiscoveryState::Ready,
        activity: 0,
    };

    apply(&mut session, Event::BackgroundTasks(snapshot("other")));

    assert!(session.background_tasks().is_none());

    apply(&mut session, Event::BackgroundTasks(snapshot("current")));

    assert!(session.background_tasks().is_some());

    session.starting(None);

    assert!(matches!(
        session.apply_event(epoch, Event::BackgroundTasks(snapshot("other"))),
        SessionEffect::Unchanged
    ));
    assert_eq!(
        session.background_tasks().unwrap().parent_session,
        BackgroundTaskKey::codex("current")
    );
}

#[test]
fn output_and_interaction_are_committed_without_a_renderer() {
    let mut session = started(AgentKind::Codex, "current", vec![SendOutcome::StartedTurn]);

    send(&mut session, "prompt");

    let generation = session.runtime.epoch();

    apply(
        &mut session,
        Event::ItemStarted(Item::AgentMessage {
            id: "reply".into(),
            text: None,
            questions: None,
        }),
    );

    apply(
        &mut session,
        Event::AgentMessageDelta {
            item_id: "reply".into(),
            delta: "answer".into(),
        },
    );

    apply(
        &mut session,
        Event::ApprovalRequested {
            description: "approve".into(),
        },
    );

    assert_eq!(
        session
            .conversation
            .borrow()
            .content
            .latest_agent_message(1),
        Some("answer")
    );
    assert_eq!(session.input.approval(), Some("approve"));
    assert!(session.interrupt_from_user().prompt.is_none());

    let version = session.conversation.borrow().version();

    session.starting(None);

    session.apply_event(
        generation,
        Event::AgentMessageDelta {
            item_id: "reply".into(),
            delta: "stale".into(),
        },
    );

    assert_eq!(session.conversation.borrow().version(), version);
    assert_eq!(
        session
            .conversation
            .borrow()
            .content
            .latest_agent_message(1),
        Some("answer")
    );
}

#[test]
fn repeated_ready_preserves_the_running_turn_and_selected_settings() {
    let mut session = started(AgentKind::Claude, "current", vec![SendOutcome::StartedTurn]);

    session.controls.settings.effort = Some("high".into());
    send(&mut session, "prompt");
    apply(&mut session, Event::TurnStarted);

    let started = session.conversation.borrow().live.started();

    apply(&mut session, Event::Ready(ThreadSettings::default()));

    assert_eq!(session.runtime.status(), Status::Running);
    assert_eq!(session.delivery.turn(), 1);
    assert_eq!(session.controls.settings.effort.as_deref(), Some("high"));
    assert_eq!(session.conversation.borrow().live.started(), started);
    assert_eq!(session.conversation.borrow().content.entries().len(), 1);
}

#[test]
fn settings_changes_and_restart_keep_catalog_state_consistent() {
    let mut session = started(AgentKind::Codex, "current", Vec::new());

    apply(
        &mut session,
        Event::Models(vec![ModelInfo {
            model: "selected".into(),
            display: "Selected".into(),
            tiers: vec![("fast".into(), "Fast".into())],
            default_tier: None,
            efforts: Vec::new(),
        }]),
    );

    apply(&mut session, Event::Commands(Vec::new()));

    session.set_tier(Some("unavailable".into()));
    session.set_model("selected".into());

    assert_eq!(
        session.controls().settings.model.as_deref(),
        Some("selected")
    );
    assert!(session.controls().settings.tier.is_none());

    session.set_tier(Some("fast".into()));
    session.set_model("selected".into());

    assert_eq!(session.controls().settings.tier.as_deref(), Some("fast"));
    assert!(session.command_catalog().is_some());

    session.seed_settings(SettingsSeed::Reviewer);
    session.begin_branched_conversation();

    assert_eq!(session.controls().seed, SettingsSeed::None);
    assert!(session.reset_for_restart().is_some());
    assert_eq!(session.controls().settings, ThreadSettings::default());
    assert!(session.controls().models.is_empty());
    assert!(session.command_catalog().is_none());
    assert!(session.skill_catalog().is_none());
}
