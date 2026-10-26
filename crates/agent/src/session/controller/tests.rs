use std::time::{Duration, Instant, SystemTime};

use crate::background_task::{BackgroundTaskKey, BackgroundTaskLoadState, BackgroundTaskSnapshot};
use crate::chat::{
    Event, ForkAnchor, ForkCheckpoint, GenerationSample, Item, ModelInfo, Question, QuestionInput,
    QuestionMode, QuestionRequest, QuestionResolution, SendOutcome, SessionSummary,
    SlashCommandOutcome, ThreadSettings,
};
use crate::progress::{GoalStatus, Task, TaskList, TaskStatus};
use crate::session::branch::BranchView;
use crate::session::command::{
    AgentCommand as _, Prompt, PromptImage, SubmitPrompt, SubmitRefusal, Submitted,
};
use crate::session::controller::{SessionController, SessionEffect};
use crate::session::delivery::RecoverablePrompt;
use crate::session::input::{ApprovalOutcome, QuestionAction, QuestionKey, Submission};
use crate::session::lifecycle::{InterruptOutcome, StartOutcome, Status};
use crate::session::restore::SettingsSeed;
use crate::session::test_support::TestBackend;
use crate::session::view::{AgentView, ViewOp, ViewPublisher};
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

    let cumulative = session
        .conversation
        .borrow()
        .generation_stats
        .session_speed()
        .unwrap();

    assert!((cumulative.tokens_per_second - 230.0 / 3.0).abs() < 0.001);

    session.conversation.borrow_mut().clear();

    assert!(
        session
            .conversation
            .borrow()
            .generation_stats
            .session_speed()
            .is_none()
    );

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

#[test]
fn completing_a_task_writes_the_list_into_the_transcript_once_per_completion() {
    let mut session = started(AgentKind::Codex, "task-snapshots", vec![]);

    let epoch = session.runtime.epoch();

    let task = |id: &str, status: TaskStatus| Task {
        id: id.into(),
        title: format!("Task {id}"),
        status,
        description: None,
        owner: None,
        blocked_by: Vec::new(),
    };

    let list = |statuses: [TaskStatus; 2]| TaskList {
        items: vec![task("one", statuses[0]), task("two", statuses[1])],
        explanation: None,
    };

    let snapshots = |session: &SessionController| {
        session
            .conversation
            .borrow()
            .content
            .entries()
            .iter()
            .filter(|entry| matches!(entry.item, Item::TaskList { .. }))
            .count()
    };

    // A list that arrives complete on resume, or is merely rearranged, is
    // panel state alone.
    session.apply_event(
        epoch,
        Event::TaskListUpdated(list([TaskStatus::Completed, TaskStatus::Pending])),
    );

    session.apply_event(
        epoch,
        Event::TaskListUpdated(list([TaskStatus::Completed, TaskStatus::InProgress])),
    );

    assert_eq!(snapshots(&session), 0);

    session.apply_event(
        epoch,
        Event::TaskListUpdated(list([TaskStatus::Completed, TaskStatus::Completed])),
    );

    assert_eq!(snapshots(&session), 1);

    assert!(matches!(
        session.conversation.borrow().content.entries().last().map(|entry| &entry.item),
        Some(Item::TaskList { tasks, .. }) if tasks.all_completed()
    ));

    // Restating the finished list completes nothing further.
    session.apply_event(
        epoch,
        Event::TaskListUpdated(list([TaskStatus::Completed, TaskStatus::Completed])),
    );

    assert_eq!(snapshots(&session), 1);
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
                    image_paths: &[],
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
fn deepseek_child_snapshots_reach_their_parent() {
    let mut session = started(AgentKind::DeepSeek, "current", Vec::new());

    apply(
        &mut session,
        Event::BackgroundTasks(BackgroundTaskSnapshot {
            parent_session: BackgroundTaskKey::deepseek("current"),
            tasks: Vec::new(),
            discovery: BackgroundTaskLoadState::Ready,
            activity: 0,
        }),
    );

    assert_eq!(
        session.background_tasks().unwrap().parent_session,
        BackgroundTaskKey::deepseek("current")
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
    // which is kept instead of replaced by the remembered pick.
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
    // pick is seeded, and a Ready with no composition keeps it.
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

/// Apply what a publisher sent, as a view in another process does.
fn follow(replica: &mut SessionController, ops: Vec<ViewOp>) {
    for op in ops {
        match op {
            ViewOp::Splice { from, entries } => replica.splice_transcript(
                from,
                entries
                    .into_iter()
                    .map(|entry| entry.into_entry(Vec::new()))
                    .collect(),
            ),
            ViewOp::Slot { slot } => replica.apply_slot(*slot),
        }
    }
}

fn through_json(ops: Vec<ViewOp>) -> Vec<ViewOp> {
    serde_json::from_value(serde_json::to_value(ops).unwrap()).unwrap()
}

fn assert_in_step(host: &SessionController, replica: &SessionController) {
    assert_eq!(replica.view_slots(), host.view_slots());
    assert_eq!(replica.transcript_view(0), host.transcript_view(0));
}

#[test]
fn a_replica_follows_a_conversation_through_published_changes() {
    let mut host = started(AgentKind::Codex, "view", vec![SendOutcome::StartedTurn]);
    let mut replica = SessionController::new(AgentKind::Codex);
    let mut publisher = ViewPublisher::default();

    send(&mut host, "first prompt");

    // Views are handed over as parsed JSON values, the form a transport
    // decodes them from.
    let json = serde_json::to_value(publisher.snapshot(&host)).unwrap();

    follow(
        &mut replica,
        serde_json::from_value::<AgentView>(json)
            .unwrap()
            .into_ops(),
    );

    assert_in_step(&host, &replica);

    apply(&mut host, Event::TurnStarted);

    apply(
        &mut host,
        Event::ItemStarted(Item::AgentMessage {
            id: "reply".into(),
            text: None,
            questions: None,
        }),
    );

    apply(
        &mut host,
        Event::AgentMessageDelta {
            item_id: "reply".into(),
            delta: "streamed ".into(),
        },
    );

    apply(
        &mut host,
        Event::ApprovalRequested {
            description: "Allow command".into(),
        },
    );

    follow(&mut replica, through_json(publisher.changes(&host)));

    assert_in_step(&host, &replica);

    assert_eq!(replica.input().approval(), Some("Allow command"));
    assert!(replica.conversation().borrow().live.is_working());

    // Streaming resends only the entry being written.
    apply(
        &mut host,
        Event::AgentMessageDelta {
            item_id: "reply".into(),
            delta: "text".into(),
        },
    );

    let ops = publisher.changes(&host);

    assert!(matches!(&ops[..], [ViewOp::Splice { from: 1, entries }] if entries.len() == 1));

    follow(&mut replica, through_json(ops));

    apply(&mut host, Event::TurnCompleted { error: None });

    follow(&mut replica, through_json(publisher.changes(&host)));

    assert_in_step(&host, &replica);

    assert_eq!(replica.runtime().status(), Status::Idle);

    // Time passing on its own is not a change.
    assert!(publisher.changes(&host).is_empty());
}

#[test]
fn a_view_applying_published_changes_equals_a_fresh_snapshot() {
    let mut host = started(AgentKind::Codex, "view", vec![SendOutcome::StartedTurn]);
    let mut publisher = ViewPublisher::default();

    send(&mut host, "first prompt");

    let mut view: AgentView =
        serde_json::from_value(serde_json::to_value(publisher.snapshot(&host)).unwrap()).unwrap();

    let steps = [
        Event::TurnStarted,
        Event::ItemStarted(Item::AgentMessage {
            id: "reply".into(),
            text: None,
            questions: None,
        }),
        Event::AgentMessageDelta {
            item_id: "reply".into(),
            delta: "streamed ".into(),
        },
        Event::ApprovalRequested {
            description: "Allow command".into(),
        },
        Event::AgentMessageDelta {
            item_id: "reply".into(),
            delta: "text".into(),
        },
        Event::TurnCompleted { error: None },
    ];

    // One change set per event, so streaming splices and slot replacements
    // interleave the way they do on a live link.
    for event in steps {
        apply(&mut host, event);

        for op in through_json(publisher.changes(&host)) {
            view.apply(op);
        }

        assert_eq!(view, ViewPublisher::default().snapshot(&host));
    }
}

#[test]
fn a_replica_shows_the_host_branch_picker_until_it_closes() {
    let mut host = started(AgentKind::Codex, "view", Vec::new());
    let mut replica = SessionController::new(AgentKind::Codex);
    let mut publisher = ViewPublisher::default();

    if let Some(Backend::Test(backend)) = host.runtime.backend_mut() {
        backend.fork_accepted = true;
    }

    follow(&mut replica, publisher.snapshot(&host).into_ops());

    host.begin_fork(None).unwrap();

    let checkpoint = ForkCheckpoint {
        prompt: "first prompt".into(),
        timestamp: None,
        anchor: ForkAnchor::CodexThrough("turn-1".into()),
    };

    apply(
        &mut host,
        Event::ForkCheckpoints(Ok(vec![checkpoint.clone()])),
    );

    follow(&mut replica, through_json(publisher.changes(&host)));

    assert_in_step(&host, &replica);

    // The replica shows the host's rows and keeps its composer held, but
    // runs nothing of its own.
    assert!(matches!(
        BranchView::from(replica.branch()),
        BranchView::ForkCheckpoints([row]) if *row == checkpoint
    ));
    assert!(replica.branch().holds_composer());

    assert!(host.cancel_branch_picker());

    follow(&mut replica, through_json(publisher.changes(&host)));

    assert!(!replica.branch().holds_composer());
}

#[test]
fn a_replica_receives_the_conversations_the_host_listed_for_it() {
    let mut host = started(AgentKind::Codex, "view", Vec::new());
    let mut replica = SessionController::new(AgentKind::Codex);
    let mut publisher = ViewPublisher::default();

    follow(&mut replica, publisher.snapshot(&host).into_ops());

    let summary = |id: &str| SessionSummary {
        id: id.into(),
        title: id.into(),
        branch: None,
        cwd: None,
        last_active: SystemTime::UNIX_EPOCH,
        snippet: None,
        origin: None,
    };

    // Protocol pages add up; a new listing starts over.
    apply(&mut host, Event::History(vec![summary("a")]));
    apply(&mut host, Event::History(vec![summary("b")]));

    follow(&mut replica, through_json(publisher.changes(&host)));

    assert_eq!(replica.listed_history(), [summary("a"), summary("b")]);

    host.clear_listed_history();
    host.list_history(vec![summary("c")]);

    follow(&mut replica, through_json(publisher.changes(&host)));

    assert_eq!(replica.listed_history(), [summary("c")]);
}

#[test]
fn a_replica_distinguishes_loading_history_from_an_empty_result() {
    let mut host = started(AgentKind::Codex, "view", Vec::new());
    let mut replica = SessionController::new(AgentKind::Codex);
    let mut publisher = ViewPublisher::default();

    follow(&mut replica, publisher.snapshot(&host).into_ops());

    host.clear_listed_history();

    follow(&mut replica, through_json(publisher.changes(&host)));

    assert!(replica.view_slots().status.history_loading);

    apply(&mut host, Event::History(Vec::new()));
    follow(&mut replica, through_json(publisher.changes(&host)));

    assert!(!replica.view_slots().status.history_loading);
    assert!(replica.listed_history().is_empty());

    host.clear_listed_history();
    host.list_history(Vec::new());

    assert!(!host.view_slots().status.history_loading);

    host.clear_listed_history();

    apply(
        &mut host,
        Event::Error {
            message: "history unavailable".into(),
            fatal: false,
        },
    );

    assert!(!host.view_slots().status.history_loading);
}

fn prompt(text: &str, images: usize) -> Prompt {
    Prompt {
        text: text.into(),
        title_text: text.into(),
        fallback_title: None,
        skill: None,
        images: (0..images)
            .map(|index| PromptImage {
                bytes: vec![index as u8; 4].into(),
                media_type: "image/png".into(),
            })
            .collect(),
        image_paths: Vec::new(),
        recoverable: None,
    }
}

#[test]
fn a_submitted_prompt_claims_the_title_and_keeps_its_images() {
    let mut host = started(
        AgentKind::Claude,
        "submit",
        vec![SendOutcome::StartedTurn, SendOutcome::Steered],
    );

    let first = SubmitPrompt(prompt("Fix the parser", 1)).run(&mut host);

    assert_eq!(
        first,
        Ok(Submitted {
            started_turn: true,
            title: Some("Fix the parser".into()),
        })
    );

    let images = |host: &SessionController| {
        host.conversation()
            .borrow()
            .content
            .entries()
            .iter()
            .map(|entry| entry.metadata.images.len())
            .collect::<Vec<_>>()
    };

    assert_eq!(images(&host), vec![1]);

    // A steered prompt names nothing and holds its images until the
    // harness echoes the message.
    let second = SubmitPrompt(prompt("and the tests", 2)).run(&mut host);

    assert_eq!(
        second,
        Ok(Submitted {
            started_turn: false,
            title: None,
        })
    );

    apply(
        &mut host,
        Event::ItemStarted(Item::UserMessage {
            text: Some("and the tests".into()),
        }),
    );

    assert_eq!(images(&host), vec![1, 2]);
}

#[test]
fn a_refused_prompt_reports_why() {
    let mut host = started(
        AgentKind::Codex,
        "refused",
        vec![SendOutcome::Rejected {
            message: "offline".into(),
        }],
    );

    assert_eq!(
        SubmitPrompt(prompt("hello", 0)).run(&mut host),
        Err(SubmitRefusal::Rejected {
            message: "offline".into(),
        })
    );

    assert!(host.conversation().borrow().content.entries().is_empty());
}
