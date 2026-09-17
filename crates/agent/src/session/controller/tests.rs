use std::path::Path;
use std::time::{Duration, Instant};

use crate::background_task::{BackgroundTaskKey, BackgroundTaskLoadState, BackgroundTaskSnapshot};
use crate::chat::{
    Event, GenerationSample, Item, ModelInfo, Question, QuestionInput, QuestionMode,
    QuestionRequest, QuestionResolution, SendOutcome, SlashCommandOutcome, ThreadSettings,
};
use crate::progress::{GoalStatus, Task, TaskList, TaskStatus};
use crate::session::controller::{SessionController, SessionEffect};
use crate::session::delivery::RecoverablePrompt;
use crate::session::input::{ApprovalOutcome, QuestionAction, QuestionKey, Submission};
use crate::session::lifecycle::{InterruptOutcome, StartOutcome, Status};
use crate::session::restore::SettingsSeed;
use crate::session::test_support::TestBackend;
use crate::session::{AgentKind, Backend, PromptRequest, SettingsOutcome};

#[test]
fn generation_speed_weights_responses_ignores_duplicates_and_resets_next_turn() {
    let mut session = started(
        AgentKind::Codex,
        "speed",
        vec![SendOutcome::StartedTurn, SendOutcome::StartedTurn],
    );

    send(&mut session, "prompt");
    apply(&mut session, Event::TurnStarted);

    let sample = |id: &str, tokens, millis| {
        Event::GenerationCompleted(GenerationSample {
            response_id: id.into(),
            output_tokens: tokens,
            elapsed: Duration::from_millis(millis),
            estimated: true,
        })
    };

    assert!(matches!(
        apply(&mut session, sample("first", 100, 500)),
        SessionEffect::Changed
    ));
    assert!(matches!(
        apply(&mut session, sample("first", 100, 500)),
        SessionEffect::Unchanged
    ));

    apply(&mut session, sample("second", 100, 1500));

    assert!(matches!(
        apply(&mut session, sample("missing-time", 1000, 0)),
        SessionEffect::Unchanged
    ));

    apply(
        &mut session,
        Event::ApprovalRequested {
            description: "Allow command".into(),
        },
    );

    let speed = session
        .conversation
        .borrow()
        .generation_stats
        .speed()
        .unwrap();

    assert!((speed.tokens_per_second - 100.0).abs() < 0.001);
    assert!(speed.estimated);

    apply(&mut session, Event::TurnCompleted { error: None });

    assert!(matches!(
        apply(&mut session, sample("late", 1000, 1000)),
        SessionEffect::Unchanged
    ));
    assert!(
        (session
            .conversation
            .borrow()
            .generation_stats
            .speed()
            .unwrap()
            .tokens_per_second
            - 100.0)
            .abs()
            < 0.001
    );

    send(&mut session, "next");
    apply(&mut session, Event::TurnStarted);

    assert!(
        session
            .conversation
            .borrow()
            .generation_stats
            .speed()
            .is_none()
    );

    apply(&mut session, sample("first", 30, 1000));

    assert!(
        (session
            .conversation
            .borrow()
            .generation_stats
            .speed()
            .unwrap()
            .tokens_per_second
            - 30.0)
            .abs()
            < 0.001
    );

    session.conversation.borrow_mut().clear();

    assert!(
        session
            .conversation
            .borrow()
            .generation_stats
            .speed()
            .is_none()
    );
}

#[test]
fn progress_survives_turns_but_clears_with_the_conversation_for_every_provider() {
    for kind in [AgentKind::Codex, AgentKind::Claude, AgentKind::DeepSeek] {
        let mut session = started(kind, "progress-session", vec![]);

        let epoch = session.runtime.epoch();

        let tasks = TaskList {
            items: vec![Task {
                id: "one".into(),
                title: "Verify output".into(),
                status: TaskStatus::InProgress,
                description: None,
                owner: None,
                blocked_by: Vec::new(),
            }],
            explanation: None,
        };

        session.apply_event(epoch, Event::TaskListUpdated(tasks));

        session.apply_event(
            epoch,
            Event::GoalUpdated(Some(GoalStatus {
                objective: "Build both targets".into(),
                phase: "active".into(),
                ..GoalStatus::default()
            })),
        );

        session.apply_event(epoch, Event::TurnStarted);
        session.apply_event(epoch, Event::TurnCompleted { error: None });

        assert_eq!(session.task_list.as_ref().unwrap().tally(), Some((0, 1)));
        assert!(session.goal.is_some());

        session.apply_event(epoch, Event::TaskListUpdated(TaskList::default()));

        assert!(session.task_list.as_ref().unwrap().items.is_empty());

        session.clear_conversation();

        assert!(session.task_list.is_none());
        assert!(session.goal.is_none());
    }
}

fn started(kind: AgentKind, id: &str, outcomes: Vec<SendOutcome>) -> SessionController {
    let mut session = SessionController::new(kind);

    let epoch = session.starting(None);

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

fn send(session: &mut SessionController, text: &str) -> SendOutcome {
    session
        .submit(
            text.into(),
            |backend, text| {
                backend.submit(&PromptRequest {
                    text,
                    settings: &ThreadSettings::default(),
                    skill: None,
                    images: &[],
                    scratch: Path::new("unused-test-attachments"),
                    title: None,
                })
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

    let old = session.starting(None);

    assert_eq!(send(&mut session, "draft"), SendOutcome::NotReady);
    assert_eq!(session.delivery.turn(), 0);

    let current = session.starting(None);

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

    assert_eq!(send(&mut session, "accepted"), SendOutcome::StartedTurn);
    assert!(matches!(
        apply(&mut session, Event::TurnStarted),
        SessionEffect::TurnStarted { opened: false }
    ));
    assert_eq!(
        send(&mut session, "rejected"),
        SendOutcome::Rejected {
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
        SessionEffect::TurnCompleted { .. }
    ));
    assert!(matches!(
        apply(&mut session, Event::TurnCompleted { error: None }),
        SessionEffect::TurnCompleted { .. }
    ));
    assert!(session.conversation.borrow().turns.was_interrupted(turn));
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
fn provider_busy_input_waits_for_confirmation_across_turn_boundaries() {
    for kind in [AgentKind::Codex, AgentKind::Claude, AgentKind::DeepSeek] {
        let mut session = started(
            kind,
            "thread",
            vec![SendOutcome::StartedTurn, SendOutcome::Steered],
        );

        send(&mut session, "first");

        apply(&mut session, Event::TurnStarted);

        assert_eq!(send(&mut session, "follow-up"), SendOutcome::Steered);
        assert!(session.delivery.pop_confirmed().is_none());

        apply(&mut session, Event::TurnCompleted { error: None });

        let count = |session: &SessionController| {
            session.conversation.borrow().content.entries().iter().filter(|entry| matches!(&entry.item, Item::UserMessage { text: Some(text) } if text == "follow-up")).count()
        };

        assert_eq!(count(&session), 0);

        apply(&mut session, Event::TurnStarted);

        assert_eq!(count(&session), 0);
        assert!(session.delivery.pop_confirmed().is_none());

        for _ in 0..2 {
            apply(
                &mut session,
                Event::ItemStarted(Item::UserMessage {
                    text: Some("follow-up".into()),
                }),
            );

            assert_eq!(count(&session), 1);
            assert!(session.delivery.pending().is_empty());
        }
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
        Submission::Waiting
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
        Submission::Ignored
    ));
}

#[test]
fn child_snapshots_are_visible_only_to_the_matching_parent_and_epoch() {
    let mut session = started(AgentKind::Codex, "current", Vec::new());

    let epoch = session.runtime.epoch();

    let snapshot = |id| BackgroundTaskSnapshot {
        parent_session: BackgroundTaskKey::codex(id),
        tasks: Vec::new(),
        discovery: BackgroundTaskLoadState::Ready,
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

fn deepseek_with_remembered_permission(selection: SettingsOutcome) -> (SessionController, u64) {
    let mut session = SessionController::new(AgentKind::DeepSeek);

    let epoch = session.starting(None);

    session.controls.seed_settings(SettingsSeed::Defaults);

    session.ready_defaults.stored = Some(ThreadSettings {
        approval: Some("danger-full-access".into()),
        ..ThreadSettings::default()
    });

    let mut backend = TestBackend::new(Vec::new(), SlashCommandOutcome::NotReady, Vec::new());

    backend.approval_selection = selection;

    assert!(matches!(
        session.install(epoch, Ok(Backend::Test(backend))),
        StartOutcome::Installed
    ));

    (session, epoch)
}

fn harness_default_ready() -> Event {
    Event::Ready(ThreadSettings {
        approval: Some("workspace-write".into()),
        ..ThreadSettings::default()
    })
}

fn approval_selections(session: &SessionController) -> Vec<String> {
    match session.runtime.backend() {
        Some(Backend::Test(backend)) => backend.approval_selections.clone(),
        _ => panic!("the test backend must be installed"),
    }
}

#[test]
fn a_new_deepseek_conversation_runs_under_the_remembered_permission() {
    let (mut session, epoch) = deepseek_with_remembered_permission(SettingsOutcome::Effective);

    let SessionEffect::Ready(ready) = session.apply_event(epoch, harness_default_ready()) else {
        panic!("startup must expose effective settings");
    };

    assert_eq!(ready.approval, Some(SettingsOutcome::Effective));
    assert_eq!(approval_selections(&session), ["danger-full-access"]);
    assert_eq!(
        session.controls.settings.approval.as_deref(),
        Some("danger-full-access")
    );

    // A conversation resumed in place reports the preset its own log holds,
    // which is kept rather than replaced by the remembered pick.
    let SessionEffect::Ready(resumed) = session.apply_event(epoch, harness_default_ready()) else {
        panic!("a resumed conversation must expose effective settings");
    };

    assert!(resumed.approval.is_none());
    assert_eq!(approval_selections(&session).len(), 1);
    assert_eq!(
        session.controls.settings.approval.as_deref(),
        Some("workspace-write")
    );
}

#[test]
fn a_refused_remembered_permission_leaves_the_picker_on_the_session_preset() {
    let (mut session, epoch) = deepseek_with_remembered_permission(SettingsOutcome::Refused {
        message: "unknown preset".to_string(),
    });

    let SessionEffect::Ready(ready) = session.apply_event(epoch, harness_default_ready()) else {
        panic!("startup must expose effective settings");
    };

    assert_eq!(
        ready.approval,
        Some(SettingsOutcome::Refused {
            message: "unknown preset".to_string()
        })
    );
    assert_eq!(
        session.controls.settings.approval.as_deref(),
        Some("workspace-write")
    );
}

#[test]
fn picks_that_ride_the_next_submission_stay_on_the_pickers() {
    let mut session = SessionController::new(AgentKind::Codex);

    let epoch = session.starting(None);

    session.controls.seed_settings(SettingsSeed::Defaults);

    session.ready_defaults.stored = Some(ThreadSettings {
        model: Some("remembered-model".into()),
        approval: Some("danger-full-access".into()),
        ..ThreadSettings::default()
    });

    // The test backend answers like a harness that sends nothing for a pick
    // and reports no selection of its own.
    let backend = TestBackend::new(Vec::new(), SlashCommandOutcome::NotReady, Vec::new());

    assert!(matches!(
        session.install(epoch, Ok(Backend::Test(backend))),
        StartOutcome::Installed
    ));

    let SessionEffect::Ready(ready) = session.apply_event(epoch, harness_default_ready()) else {
        panic!("startup must expose effective settings");
    };

    assert_eq!(ready.selection, Some(SettingsOutcome::RidesNextSubmission));
    assert_eq!(ready.approval, Some(SettingsOutcome::RidesNextSubmission));

    // Nothing answered for the session, so its empty selection must not
    // replace what the user chose.
    assert_eq!(
        session.controls.settings.model.as_deref(),
        Some("remembered-model")
    );
    assert_eq!(
        session.controls.settings.approval.as_deref(),
        Some("danger-full-access")
    );
}

#[test]
fn a_remembered_agent_preset_never_overrides_the_reported_composition() {
    let (mut session, epoch) = deepseek_with_remembered_permission(SettingsOutcome::Effective);

    session.ready_defaults.stored = Some(ThreadSettings {
        agent_preset: Some("reviewer".into()),
        ..ThreadSettings::default()
    });

    session.apply_event(
        epoch,
        Event::AgentPresets {
            presets: Vec::new(),
            current: Some("default".into()),
        },
    );

    // The composition reaches the harness with the creation request, so the
    // one the conversation reports is what it runs on even while a remembered
    // pick is seeded, and a Ready carrying no composition keeps it.
    assert!(matches!(
        session.apply_event(epoch, harness_default_ready()),
        SessionEffect::Ready(_)
    ));
    assert_eq!(
        session.controls.settings.agent_preset.as_deref(),
        Some("default")
    );
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

    session.controls.settings.tier = Some("unavailable".into());

    session.controls.set_model("selected".into());

    assert_eq!(session.controls.settings.model.as_deref(), Some("selected"));
    assert!(session.controls.settings.tier.is_none());

    session.controls.settings.tier = Some("fast".into());

    session.controls.set_model("selected".into());

    assert_eq!(session.controls.settings.tier.as_deref(), Some("fast"));
    assert!(session.command_catalog().is_some());

    session.controls.seed_settings(SettingsSeed::Reviewer);

    session.begin_branched_conversation();

    assert_eq!(session.controls.seed, SettingsSeed::None);
    assert!(session.reset_for_restart().is_some());
    assert_eq!(session.controls.settings, ThreadSettings::default());
    assert!(session.controls.models.is_empty());
    assert!(session.command_catalog().is_none());
    assert!(session.skill_catalog().is_none());
}

#[test]
fn an_answered_model_pick_puts_the_pickers_on_what_the_session_runs() {
    let (mut session, epoch) = deepseek_with_remembered_permission(SettingsOutcome::Requested);

    session.controls.settings.model = Some("picked-model".into());
    session.controls.settings.effort = Some("max".into());

    // The harness took the model and kept the effort it chose for it.
    let taken = session.apply_event(
        epoch,
        Event::ModelSelection {
            model: Some("picked-model".into()),
            effort: Some("high".into()),
            refusal: None,
        },
    );

    assert!(matches!(taken, SessionEffect::Changed));
    assert_eq!(
        session.controls.settings.model.as_deref(),
        Some("picked-model")
    );
    assert_eq!(session.controls.settings.effort.as_deref(), Some("high"));

    session.controls.settings.model = Some("unserved-model".into());

    let refused = session.apply_event(
        epoch,
        Event::ModelSelection {
            model: Some("picked-model".into()),
            effort: Some("high".into()),
            refusal: Some("no adapter serves it".into()),
        },
    );

    assert!(matches!(
        refused,
        SessionEffect::EffortRejected { message } if message == "no adapter serves it"
    ));
    assert_eq!(
        session.controls.settings.model.as_deref(),
        Some("picked-model")
    );
}
