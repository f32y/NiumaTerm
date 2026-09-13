use std::time::Duration;

use nmt_agent::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskRegistry,
    BackgroundTaskSnapshot, BackgroundTaskState, BackgroundTaskUpdate,
};
use nmt_agent::chat::ThreadSettings;

use crate::agent_tab::AgentKind;
use crate::agent_tab::session::background_tasks::scoped_background_tasks;
use crate::agent_tab::session::events::resolve_ready_settings;
use crate::agent_tab::session::turn::replayed_response_age;
use crate::agent_tab::session::{
    conversation_title_request, directories_match, directory_label, tab_title_from_prompt,
};
use crate::agent_tab::transcript::LAST_RESPONSE_LIMIT;

fn snapshot_for(parent: BackgroundTaskKey) -> BackgroundTaskSnapshot {
    let mut registry = BackgroundTaskRegistry::new(parent);

    registry.apply(
        BackgroundTaskKey::codex("child-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );

    registry.snapshot()
}

#[test]
fn a_snapshot_is_shown_only_for_the_session_it_describes() {
    let codex = BackgroundTaskKey::codex("thread-a");
    let snapshot = snapshot_for(codex.clone());

    assert!(scoped_background_tasks(Some(&codex), Some(&snapshot)).is_some());
    assert!(
        scoped_background_tasks(Some(&BackgroundTaskKey::codex("thread-b")), Some(&snapshot))
            .is_none()
    );
    assert!(
        scoped_background_tasks(
            Some(&BackgroundTaskKey::claude_code("thread-a")),
            Some(&snapshot)
        )
        .is_none(),
        "a Claude session must not adopt a Codex thread's rows"
    );
    assert!(
        scoped_background_tasks(None, Some(&snapshot)).is_none(),
        "an unsupported or not-yet-started pane shows no rows"
    );
}

#[test]
fn a_later_snapshot_replaces_the_previous_one_and_carries_its_activity() {
    let parent = BackgroundTaskKey::claude_code("session-1");
    let mut registry = BackgroundTaskRegistry::new(parent.clone());

    registry.apply(
        BackgroundTaskKey::claude_code("task-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );

    let first = registry.snapshot();

    registry.apply(
        BackgroundTaskKey::claude_code("task-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Done),
    );

    let second = registry.snapshot();

    assert_eq!(first.active_count(), 1);
    assert_eq!(second.active_count(), 0);
    assert!(second.activity > first.activity);
    assert_eq!(
        scoped_background_tasks(Some(&parent), Some(&second)),
        Some(&second)
    );
}

#[test]
fn a_failed_refresh_reports_unavailable_without_dropping_known_rows() {
    let parent = BackgroundTaskKey::codex("thread-a");
    let mut registry = BackgroundTaskRegistry::new(parent);

    registry.apply(
        BackgroundTaskKey::codex("child-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );

    registry.set_discovery(BackgroundTaskDiscoveryState::Unavailable {
        message: "thread/list failed".into(),
    });

    let snapshot = registry.snapshot();

    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.active_count(), 1);
    assert!(matches!(
        snapshot.discovery,
        BackgroundTaskDiscoveryState::Unavailable { .. }
    ));
}

#[test]
fn resumed_codex_thread_uses_only_the_locally_remembered_reviewer() {
    let backend = ThreadSettings {
        model: Some("thread-model".into()),
        approval: Some("never".into()),
        approvals_reviewer: Some("user".into()),
        sandbox: Some("readOnly".into()),
        effort: Some("low".into()),
        tier: Some("priority".into()),
    };

    let stored = ThreadSettings {
        model: Some("local-model".into()),
        approval: Some("on-request".into()),
        approvals_reviewer: Some("auto_review".into()),
        sandbox: Some("workspaceWrite".into()),
        effort: Some("high".into()),
        tier: None,
    };

    assert_eq!(
        resolve_ready_settings(backend, Some(&stored), false, true, None, None),
        ThreadSettings {
            model: Some("thread-model".into()),
            approval: Some("never".into()),
            approvals_reviewer: Some("auto_review".into()),
            sandbox: Some("readOnly".into()),
            effort: Some("low".into()),
            tier: Some("priority".into()),
        }
    );
}

#[test]
fn claude_profile_and_local_settings_survive_later_ready_events() {
    let backend = ThreadSettings {
        model: Some("agent-model".into()),
        approval: Some("default".into()),
        effort: None,
        ..ThreadSettings::default()
    };

    let local = ThreadSettings {
        model: Some("remembered-model".into()),
        approval: Some("auto".into()),
        effort: Some("high".into()),
        ..ThreadSettings::default()
    };

    let initial = resolve_ready_settings(
        backend.clone(),
        Some(&local),
        true,
        false,
        Some("profile-model"),
        None,
    );

    assert_eq!(initial.model.as_deref(), Some("profile-model"));
    assert_eq!(initial.approval.as_deref(), Some("auto"));
    assert_eq!(initial.effort.as_deref(), Some("high"));
    assert_eq!(
        resolve_ready_settings(backend, Some(&initial), true, false, None, None),
        initial
    );
}

#[test]
fn a_pinned_profile_effort_outranks_the_thread_and_the_remembered_pick() {
    let backend = ThreadSettings {
        effort: Some("low".into()),
        ..ThreadSettings::default()
    };

    let local = ThreadSettings {
        effort: Some("medium".into()),
        ..ThreadSettings::default()
    };

    let resolved = resolve_ready_settings(backend, Some(&local), true, false, None, Some("max"));

    assert_eq!(resolved.effort.as_deref(), Some("max"));
}

#[test]
fn no_pinned_effort_leaves_the_remembered_pick_in_place() {
    let backend = ThreadSettings {
        effort: Some("low".into()),
        ..ThreadSettings::default()
    };

    let local = ThreadSettings {
        effort: Some("medium".into()),
        ..ThreadSettings::default()
    };

    let resolved = resolve_ready_settings(backend, Some(&local), true, false, None, None);

    assert_eq!(resolved.effort.as_deref(), Some("medium"));
}

#[test]
fn a_prompt_names_its_tab_by_its_first_real_line() {
    assert_eq!(
        tab_title_from_prompt(
            "
  Fix the flaky auth test
and the retry loop"
        ),
        Some("Fix the flaky auth test".to_string())
    );

    // A slash command instructs the CLI instead of stating a subject, and the
    // settings controls send some of them for the user.
    assert_eq!(tab_title_from_prompt("/effort high"), None);
    assert_eq!(tab_title_from_prompt("   \n\t "), None);

    let long = "x".repeat(200);

    assert_eq!(
        tab_title_from_prompt(&long).map(|t| t.chars().count()),
        Some(60)
    );
}

#[test]
fn title_requests_keep_each_provider_semantics() {
    let codex = conversation_title_request(
        AgentKind::Codex,
        "  Inspect title generation\n and its fallback  ",
    )
    .unwrap();

    assert_eq!(
        codex.provisional_title,
        "Inspect title generation and its fallback"
    );

    let claude = conversation_title_request(
        AgentKind::Claude,
        "  Inspect title generation\n and its fallback  ",
    )
    .unwrap();

    assert_eq!(
        claude.provisional_title,
        "Inspect title generation and its fallback"
    );
    assert!(conversation_title_request(AgentKind::Codex, "/effort high").is_none());
    assert!(conversation_title_request(AgentKind::Claude, "/effort high").is_none());
}

#[test]
fn claude_provisional_titles_match_the_desktop_projection() {
    assert_eq!(
        conversation_title_request(
            AgentKind::Claude,
            "  one two\nthree four five six seven eight  "
        )
        .as_ref()
        .map(|request| request.provisional_title.as_str()),
        Some("one two three four five six")
    );

    let long_word = "界".repeat(80);

    let title = conversation_title_request(AgentKind::Claude, &long_word)
        .unwrap()
        .provisional_title;

    assert_eq!(title.chars().count(), 60);
    assert!(title.ends_with('…'));
}

#[test]
#[cfg(windows)]
fn a_recorded_directory_is_matched_against_the_tab_across_writers() {
    // The tab's configuration and the agent's own record disagree about
    // separators and case, and neither is wrong.
    assert!(directories_match(
        Some(r"C:\Workspace\NiumaTerm"),
        Some("c:/workspace/niumaterm")
    ));
    assert!(directories_match(
        Some(r"C:\Workspace\NiumaTerm\"),
        Some(r"C:\Workspace\NiumaTerm")
    ));
    assert!(!directories_match(Some(r"C:\A"), Some(r"C:\B")));

    // A row that records nothing claims nothing, so it stays resumable here.
    assert!(directories_match(None, Some(r"C:\A")));
}

#[test]
#[cfg(unix)]
fn recorded_unix_directories_preserve_case_and_backslashes() {
    assert!(!directories_match(Some("/work/Foo"), Some("/work/foo")));
    assert!(!directories_match(Some(r"/work/a\b"), Some("/work/a/b")));
    assert!(directories_match(Some("/work/Foo/"), Some("/work/Foo")));
    assert!(!directories_match(Some("/"), Some("")));
    assert!(directories_match(None, Some("/work/Foo")));
}

#[test]
fn a_directory_reads_as_its_last_two_components() {
    assert_eq!(
        directory_label(r"C:\Workspace\NiumaTerm"),
        "Workspace/NiumaTerm"
    );
    assert_eq!(directory_label("/home/u/projects/app/"), "projects/app");
    assert_eq!(directory_label("C:/only"), "C:/only");
}

#[test]
fn a_replayed_answer_is_read_as_old_as_the_provider_recorded_it() {
    let now = 1_700_000_000;

    assert_eq!(
        replayed_response_age(now - 20 * 60, now),
        Duration::from_secs(20 * 60),
        "a stamp from twenty minutes ago reads as twenty minutes of idling"
    );
    assert_eq!(
        replayed_response_age(now - 5 * 24 * 60 * 60, now),
        LAST_RESPONSE_LIMIT,
        "conversations older than the label's longest reading all read the same"
    );
    assert_eq!(
        replayed_response_age(now + 30, now),
        Duration::ZERO,
        "a clock the provider ran ahead of never reads as a future answer"
    );
}

mod conversation_title_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{
        AppContext as _, Entity, Subscription, TestAppContext, VisualTestContext, WindowHandle,
    };
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{SendOutcome, SlashCommandOutcome};
    use nmt_agent::session::lifecycle::StartOutcome;
    use nmt_config::profile::{AgentProfile, AgentProfileKind};

    use crate::agent_tab::session::{Backend, RecoveryIdentity, Status, TestBackend};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::{AgentKind, AgentPane, AgentPaneEvent, AgentThreadDefaults};

    fn open_pane(
        cx: &mut TestAppContext,
        kind: AgentProfileKind,
        resume: Option<RecoveryIdentity>,
    ) -> (Entity<AgentPane>, WindowHandle<gpui_component::Root>) {
        let profile = AgentProfile {
            name: "Conversation Title Test".into(),
            kind,
            // The test replaces the asynchronous launch before submitting.
            executable: "missing-agent.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AgentSettings::default());
            cx.set_global(AgentThreadDefaults::default());

            cx.open_window(Default::default(), |window, cx| {
                let agent = cx.new(|cx| {
                    AgentPane::new_resuming(profile, AgentWorkspace::default(), resume, window, cx)
                });

                pane = Some(agent.clone());

                cx.new(|cx| gpui_component::Root::new(agent, window, cx))
            })
            .expect("open Agent test window")
        });

        (pane.expect("create Agent pane"), window)
    }

    #[gpui::test]
    fn command_catalog_rebuilds_when_the_cached_language_changes(cx: &mut TestAppContext) {
        let (pane, _) = open_pane(cx, AgentProfileKind::Codex, None);

        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                let initial = pane.command_catalog();

                assert!(Rc::ptr_eq(&initial, &pane.command_catalog()));

                pane.palette.catalog.as_mut().unwrap().language = "previous-language".into();

                let refreshed = pane.command_catalog();

                assert!(!Rc::ptr_eq(&initial, &refreshed));
                assert_eq!(initial.as_ref(), refreshed.as_ref());
                assert_eq!(
                    pane.palette.catalog.as_ref().unwrap().language,
                    &*rust_i18n::locale()
                );
            });
        });
    }

    #[gpui::test]
    fn output_failure_retires_backend_and_marks_session_exited(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx, AgentProfileKind::Codex, None);
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [],
                            SlashCommandOutcome::NotReady,
                            vec![],
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime.turn_started();
                pane.stop_for_output_failure("Output limit reached".into(), cx);

                assert!(pane.session.borrow().runtime.backend().is_none());
                assert_eq!(pane.session.borrow().runtime.status(), Status::Exited);
            });
        });
    }

    #[gpui::test]
    fn rejected_rename_keeps_latest_name_until_admitted(cx: &mut TestAppContext) {
        use nmt_agent::session::RenameOutcome;

        use crate::agent_tab::profile::AgentKind;

        let (pane, window) = open_pane(cx, AgentProfileKind::Codex, None);
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                let mut backend = TestBackend::new([], SlashCommandOutcome::NotReady, vec![])
                    .with_recovery(AgentKind::Codex, "thread");

                backend.rename_outcome = RenameOutcome::Rejected;

                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session
                        .borrow_mut()
                        .runtime
                        .install(epoch, Ok(Backend::Test(backend))),
                    StartOutcome::Installed
                ));

                pane.rename_session("first");
                pane.rename_session("latest");

                assert_eq!(
                    pane.session.borrow().naming.pending.as_deref(),
                    Some("latest")
                );

                pane.sync_pending_rename();

                assert_eq!(
                    pane.session.borrow().naming.pending.as_deref(),
                    Some("latest")
                );

                let mut state = pane.session.borrow_mut();

                let Some(Backend::Test(backend)) = state.runtime.backend_mut() else {
                    panic!("expected test backend");
                };

                backend.rename_outcome = RenameOutcome::Accepted;
                drop(state);
                pane.sync_pending_rename();

                assert!(pane.session.borrow().naming.pending.is_none());

                let mut state = pane.session.borrow_mut();

                let Some(Backend::Test(backend)) = state.runtime.backend_mut() else {
                    panic!("expected test backend");
                };

                backend.rename_outcome = RenameOutcome::Unsupported;
                drop(state);
                pane.rename_session("local only");

                assert!(pane.session.borrow().naming.pending.is_none());
            });
        });
    }

    fn collect_titles(
        pane: &Entity<AgentPane>,
        cx: &mut VisualTestContext,
    ) -> (Rc<RefCell<Vec<String>>>, Subscription) {
        let titles = Rc::new(RefCell::new(Vec::new()));
        let observed = Rc::clone(&titles);

        let subscription = cx.update(|_, cx| {
            cx.subscribe(pane, move |_, event: &AgentPaneEvent, _| {
                if let AgentPaneEvent::TitleSuggested(title) = event {
                    observed.borrow_mut().push(title.clone());
                }
            })
        });

        (titles, subscription)
    }

    #[gpui::test]
    fn rejected_control_replies_keep_interaction_cards(cx: &mut TestAppContext) {
        use nmt_agent::chat::{Event, Question, QuestionInput, QuestionMode, QuestionRequest};

        let (pane, window) = open_pane(cx, AgentProfileKind::Codex, None);
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                {
                    let mut guard = pane.session.borrow_mut();
                    let state = &mut *guard;

                    state.input.restore(&mut state.runtime)
                };

                pane.on_event(
                    Event::ApprovalRequested {
                        description: "Run a command".into(),
                    },
                    cx,
                );

                pane.respond_approval("accept", cx);

                assert!(pane.session.borrow().input.approval().is_some());

                pane.on_event(Event::ApprovalResolved, cx);
                pane.session.borrow_mut().runtime.ready();
                pane.restore_question_drafts();

                if let Some(Backend::Test(backend)) =
                    pane.session.borrow_mut().runtime.backend_mut()
                {
                    backend.input_result = Err("The question response could not be queued.".into());
                }

                pane.on_event(
                    Event::InputRequested(QuestionRequest {
                        id: "declined".into(),
                        mode: QuestionMode::Blocking,
                        questions: vec![Question {
                            input: QuestionInput::Text,
                            header: None,
                            question: "Describe the change".into(),
                            multi_select: false,
                            options: Vec::new(),
                        }],
                    }),
                    cx,
                );

                pane.skip_current_questions(cx);

                let state = pane.session.borrow();

                let question = pane
                    .prompts
                    .questions(&state.input)
                    .expect("rejected answer remains visible");

                assert!(question.error().is_some());
            });
        });
    }

    #[gpui::test]
    fn accepted_codex_prompt_publishes_a_provisional_title(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx, AgentProfileKind::Codex, None);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let (titles, _subscription) = collect_titles(&pane, &mut cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime.ready();

                assert!(pane.send_text_inner(
                    "  Inspect title generation\n and its fallback  ".into(),
                    None,
                    None,
                    cx
                ));
                assert!(pane.session.borrow().naming.named);
            });
        });

        cx.run_until_parked();

        assert_eq!(
            *titles.borrow(),
            vec!["Inspect title generation and its fallback".to_string()]
        );
    }

    #[gpui::test]
    fn accepted_claude_prompt_publishes_one_provisional_title(cx: &mut TestAppContext) {
        // Starting the test fixture as Codex avoids a real Claude subprocess;
        // the installed test backend below owns all message behavior.
        let (pane, window) = open_pane(cx, AgentProfileKind::Codex, None);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let (titles, _subscription) = collect_titles(&pane, &mut cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                pane.kind = AgentKind::Claude;

                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn, SendOutcome::Steered],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime.ready();

                assert!(pane.send_text_inner(
                    "one two three four five six seven eight".into(),
                    None,
                    None,
                    cx
                ));
                assert!(pane.session.borrow().naming.named);
                assert!(pane.send_text_inner(
                    "a later prompt cannot rename this".into(),
                    None,
                    None,
                    cx
                ));
            });
        });

        cx.run_until_parked();

        assert_eq!(*titles.borrow(), vec!["one two three four five six"]);
    }

    #[gpui::test]
    fn resumed_claude_prompt_does_not_enter_first_prompt_naming(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(
            cx,
            AgentProfileKind::Codex,
            Some(RecoveryIdentity::new(
                AgentKind::Codex,
                "resumed-conversation",
            )),
        );

        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let (titles, _subscription) = collect_titles(&pane, &mut cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                assert!(pane.session.borrow().naming.named);

                pane.kind = AgentKind::Claude;

                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime.ready();

                assert!(pane.send_text_inner(
                    "follow up on the restored session".into(),
                    None,
                    None,
                    cx
                ));
            });
        });

        cx.run_until_parked();

        assert!(titles.borrow().is_empty());
    }
}

/// Where a prompt appears between the moment it is submitted and the moment
/// its turn answers it. Each harness hands one over differently, and the two
/// ways of getting it wrong are drawing it in a turn that had already
/// finished writing, and drawing it twice at once.
mod queued_prompt_placement_tests {
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{
        Event as SessionEvent, Item as SessionItem, QueuedPrompt, SendOutcome, SlashCommandOutcome,
    };
    use nmt_agent::session::lifecycle::StartOutcome;
    use nmt_config::profile::{AgentProfile, AgentProfileKind};

    use crate::agent_tab::session::{Backend, Status, TestBackend};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::{AgentPane, AgentThreadDefaults};

    fn open_pane(
        cx: &mut TestAppContext,
        kind: AgentProfileKind,
    ) -> (Entity<AgentPane>, WindowHandle<gpui_component::Root>) {
        let profile = AgentProfile {
            name: "Queued Prompt Test".into(),
            kind,
            // Never spawned: every test below installs a backend by hand.
            executable: "missing-agent.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AgentSettings::default());
            cx.set_global(AgentThreadDefaults::default());

            cx.open_window(Default::default(), |window, cx| {
                let agent =
                    cx.new(|cx| AgentPane::new(profile, AgentWorkspace::default(), window, cx));

                pane = Some(agent.clone());

                cx.new(|cx| gpui_component::Root::new(agent, window, cx))
            })
            .expect("open Agent test window")
        });

        (pane.expect("create Agent pane"), window)
    }

    /// Every user row as the turn it was filed under and the text it carries.
    fn user_rows(pane: &AgentPane, cx: &gpui::App) -> Vec<(u64, String)> {
        pane.transcript
            .read(cx)
            .conversation
            .borrow()
            .content
            .entries()
            .iter()
            .filter_map(|entry| match &entry.item {
                SessionItem::UserMessage { text } => {
                    Some((entry.turn, text.clone().unwrap_or_default()))
                }

                _ => None,
            })
            .collect()
    }

    #[gpui::test]
    fn a_pending_command_starts_working_on_its_turn_event_once(cx: &mut TestAppContext) {
        // Codex initialization stays on the test executor; Claude would start
        // a real stdout reader before the test backend replaces it.
        let (pane, window) = open_pane(cx, AgentProfileKind::Codex);
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime.ready();
                pane.session.borrow_mut().commands.awaiting_turn = true;

                let previous_turn = pane.session.borrow().delivery.turn();

                assert!(!pane.transcript.read(cx).is_working());

                pane.on_event(SessionEvent::TurnStarted, cx);

                assert!(!pane.session.borrow().commands.awaiting_turn);
                assert_eq!(pane.session.borrow().delivery.turn(), previous_turn + 1);
                assert_eq!(pane.session.borrow().runtime.status(), Status::Running);
                assert!(pane.transcript.read(cx).is_working());

                pane.on_event(SessionEvent::TurnStarted, cx);

                assert_eq!(
                    pane.session.borrow().delivery.turn(),
                    previous_turn + 1,
                    "a repeated event must not open another turn"
                );
            });
        });
    }

    #[gpui::test]
    fn a_queued_prompt_heads_the_turn_opened_for_it(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx, AgentProfileKind::ClaudeCode);
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn, SendOutcome::Steered],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime.ready();

                assert!(pane.send_text_inner("open the turn".into(), None, None, cx));

                pane.on_event(SessionEvent::TurnStarted, cx);

                let first_turn = pane.session.borrow().delivery.turn();

                assert!(pane.send_text_inner("queued behind it".into(), None, None, cx));

                pane.on_event(
                    SessionEvent::ItemStarted(SessionItem::AgentMessage {
                        id: "msg-1".into(),
                        text: Some("the first answer".into()),
                        questions: None,
                    }),
                    cx,
                );

                pane.on_event(SessionEvent::TurnCompleted { error: None }, cx);

                assert_eq!(
                    user_rows(pane, cx),
                    vec![(first_turn, "open the turn".to_string())],
                    "the finished turn keeps only the prompt that opened it"
                );

                // The CLI answers the held prompt in a turn nothing here sent.
                pane.on_event(SessionEvent::TurnStarted, cx);

                assert_eq!(
                    pane.session.borrow().delivery.turn(),
                    first_turn + 1,
                    "that turn is numbered"
                );
                assert_eq!(pane.session.borrow().runtime.status(), Status::Running);
                assert!(pane.transcript.read(cx).is_working());
                assert_eq!(
                    user_rows(pane, cx),
                    vec![
                        (first_turn, "open the turn".to_string()),
                        (first_turn + 1, "queued behind it".to_string()),
                    ],
                    "the held prompt heads the turn that answers it"
                );
            });
        });
    }

    /// The harness lists a prompt in its pending inbox from the moment it
    /// accepts it until the turn claims it, and the send that started that
    /// turn has already drawn the prompt's row. Listing it above the composer
    /// as well would show the same message twice for that whole window.
    #[gpui::test]
    fn a_pending_inbox_omits_the_prompt_its_send_already_drew(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx, AgentProfileKind::DeepSeek);
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime.ready();

                let text = "Reply with exactly: ok".to_string();

                assert!(pane.send_text_inner(text.clone(), None, None, cx));

                // What the harness reports, in the order it reports it: the
                // prompt queued, the turn opened, the queue emptied, and the
                // harness's own echo of the message it took.
                pane.on_event(
                    SessionEvent::QueuedPrompts(vec![QueuedPrompt {
                        id: Some("afd4d197".into()),
                        text: text.clone(),
                    }]),
                    cx,
                );

                assert!(
                    pane.session.borrow().delivery.pending().is_empty(),
                    "a prompt already in the transcript is not also waiting"
                );

                pane.on_event(SessionEvent::TurnStarted, cx);
                pane.on_event(SessionEvent::QueuedPrompts(Vec::new()), cx);

                pane.on_event(
                    SessionEvent::ItemStarted(SessionItem::UserMessage {
                        text: Some(text.clone()),
                    }),
                    cx,
                );

                assert_eq!(
                    user_rows(pane, cx),
                    vec![(1, text)],
                    "the message appears once, in the turn it opened"
                );
                assert!(pane.session.borrow().delivery.pending().is_empty());
            });
        });
    }
}

mod turn_error_tests {
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{Event as SessionEvent, Item as SessionItem};
    use nmt_config::profile::{AgentProfile, AgentProfileKind};

    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::{AgentPane, AgentThreadDefaults};

    fn open_pane(
        cx: &mut TestAppContext,
    ) -> (Entity<AgentPane>, WindowHandle<gpui_component::Root>) {
        let profile = AgentProfile {
            name: "Turn Error Test".into(),
            kind: AgentProfileKind::Codex,
            executable: "missing-codex.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AgentSettings::default());
            cx.set_global(AgentThreadDefaults::default());

            cx.open_window(Default::default(), |window, cx| {
                let agent =
                    cx.new(|cx| AgentPane::new(profile, AgentWorkspace::default(), window, cx));

                pane = Some(agent.clone());

                cx.new(|cx| gpui_component::Root::new(agent, window, cx))
            })
            .expect("open Agent test window")
        });

        (pane.expect("create Agent pane"), window)
    }

    #[gpui::test]
    fn a_terminal_failure_does_not_repeat_an_error_already_shown(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                pane.on_event(SessionEvent::TurnStarted, cx);

                pane.on_event(
                    SessionEvent::ItemStarted(SessionItem::AgentMessage {
                        id: "message".into(),
                        text: Some("partial answer".into()),
                        questions: None,
                    }),
                    cx,
                );

                pane.on_event(
                    SessionEvent::Error {
                        message: "model unavailable".into(),
                        fatal: false,
                    },
                    cx,
                );

                pane.on_event(
                    SessionEvent::TurnCompleted {
                        error: Some("model unavailable".into()),
                    },
                    cx,
                );

                let conversation = pane.transcript.read(cx).conversation.borrow();

                let errors = conversation
                    .content
                    .entries()
                    .iter()
                    .filter_map(|entry| match &entry.item {
                        SessionItem::Error { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();

                assert_eq!(errors, vec!["model unavailable"]);
            });
        });
    }
}

/// What `/new` does with the session it replaces. The DeepSeek host is one
/// process shared by every tab holding a session on it, so releasing the old
/// session before the replacement has taken its own hold stops the host
/// whenever this is the only tab using it.
mod session_replacement_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use gpui::{AppContext as _, TestAppContext, VisualTestContext};
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{SendOutcome, SlashCommandOutcome};
    use nmt_agent::session::lifecycle::StartOutcome;
    use nmt_config::profile::{AgentProfile, AgentProfileKind};

    use crate::agent_tab::session::{Backend, TestBackend};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::{AgentPane, AgentThreadDefaults};

    #[gpui::test]
    fn a_reset_holds_its_old_session_until_the_replacement_is_installed(cx: &mut TestAppContext) {
        let profile = AgentProfile {
            name: "Session Replacement Test".into(),
            kind: AgentProfileKind::DeepSeek,
            // The replacement start never reaches a process: the spawn runs on
            // the background executor, which this test does not run.
            executable: "missing-agent.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AgentSettings::default());
            cx.set_global(AgentThreadDefaults::default());

            cx.open_window(Default::default(), |window, cx| {
                let agent =
                    cx.new(|cx| AgentPane::new(profile, AgentWorkspace::default(), window, cx));

                pane = Some(agent.clone());

                cx.new(|cx| gpui_component::Root::new(agent, window, cx))
            })
            .expect("open Agent test window")
        });

        let pane = pane.expect("create Agent pane");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let released = Arc::new(AtomicBool::new(false));

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(
                            TestBackend::new(
                                [SendOutcome::StartedTurn],
                                SlashCommandOutcome::NotReady,
                                Vec::new(),
                            )
                            .watch_release(released.clone()),
                        ))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime.ready();

                pane.reset_conversation(cx);

                assert!(
                    pane.session.borrow().runtime.backend().is_none(),
                    "the pane sends nowhere"
                );
                assert!(
                    !released.load(Ordering::SeqCst),
                    "the replaced session outlives the reset, so the host it holds keeps running"
                );
            });
        });
    }
}

mod shared_host_recovery_tests {
    use gpui::{AppContext as _, TestAppContext, VisualTestContext};
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{Event as SessionEvent, SendOutcome, SlashCommandOutcome};
    use nmt_agent::session::lifecycle::StartOutcome;
    use nmt_config::profile::{AgentProfile, AgentProfileKind};

    use crate::agent_tab::session::{Backend, Status, TestBackend, UpdateSuspension};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::{AgentKind, AgentPane, AgentThreadDefaults};

    #[gpui::test]
    fn a_host_exit_retains_the_thread_for_retry(cx: &mut TestAppContext) {
        let profile = AgentProfile {
            name: "Codex Recovery Test".into(),
            kind: AgentProfileKind::Codex,
            executable: "missing-codex.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AgentSettings::default());
            cx.set_global(AgentThreadDefaults::default());

            cx.open_window(Default::default(), |window, cx| {
                let agent =
                    cx.new(|cx| AgentPane::new(profile, AgentWorkspace::default(), window, cx));

                pane = Some(agent.clone());

                cx.new(|cx| gpui_component::Root::new(agent, window, cx))
            })
            .expect("open Agent test window")
        });

        let pane = pane.expect("create Agent pane");
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(
                            TestBackend::new(
                                [SendOutcome::StartedTurn],
                                SlashCommandOutcome::NotReady,
                                Vec::new(),
                            )
                            .with_recovery(AgentKind::Codex, "thread-recovery"),
                        ))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime.ready();

                pane.on_event(
                    SessionEvent::HostExited {
                        message: "Codex app-server stopped unexpectedly".into(),
                    },
                    cx,
                );

                assert_eq!(pane.session.borrow().runtime.status(), Status::Exited);
                assert!(matches!(
                    pane.session.borrow().runtime.update_suspension(),
                    Some(UpdateSuspension::Failed(_))
                ));

                let state = pane.session.borrow();

                let snapshot = state
                    .runtime
                    .last_recovery_snapshot()
                    .expect("recovery snapshot");

                assert_eq!(snapshot.profile_name, "Codex Recovery Test");
                assert_eq!(
                    snapshot
                        .identity
                        .as_ref()
                        .map(|identity| identity.id.as_str()),
                    Some("thread-recovery")
                );
                assert!(pane.session.borrow().runtime.backend().is_some());
            });
        });
    }
}

/// The merged `/` catalog is held between frames rather than rebuilt on each
/// one, so the two ways of getting it wrong are keeping a command the harness
/// has withdrawn and never showing one it has just published.
mod command_catalog_cache_tests {
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{
        Event as SessionEvent, SendOutcome, SlashCommandArguments, SlashCommandInfo,
        SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
    };
    use nmt_agent::session::lifecycle::StartOutcome;
    use nmt_config::profile::{AgentProfile, AgentProfileKind};

    use crate::agent_tab::session::{Backend, TestBackend};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::{AgentPane, AgentThreadDefaults};

    fn open_pane(
        cx: &mut TestAppContext,
    ) -> (Entity<AgentPane>, WindowHandle<gpui_component::Root>) {
        let profile = AgentProfile {
            name: "Catalog Cache Test".into(),
            kind: AgentProfileKind::Codex,
            // Never spawned: the test publishes discovery results by hand.
            executable: "missing-agent.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AgentSettings::default());
            cx.set_global(AgentThreadDefaults::default());

            cx.open_window(Default::default(), |window, cx| {
                let agent =
                    cx.new(|cx| AgentPane::new(profile, AgentWorkspace::default(), window, cx));

                pane = Some(agent.clone());

                cx.new(|cx| gpui_component::Root::new(agent, window, cx))
            })
            .expect("open Agent test window")
        });

        (pane.expect("create Agent pane"), window)
    }

    fn discovered(name: &str) -> SlashCommandInfo {
        SlashCommandInfo {
            name: name.into(),
            description: name.into(),
            argument_hint: None,
            source: SlashCommandSource::Provider,
            arguments: SlashCommandArguments::None,
            run_policy: SlashCommandRunPolicy::Immediate,
        }
    }

    fn offers(pane: &mut AgentPane, name: &str) -> bool {
        pane.command_catalog()
            .iter()
            .any(|command| command.name == name)
    }

    #[gpui::test]
    fn discovery_replacements_reach_the_palette(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        // Replaces the launch the pane started for itself, which has no
        // executable to reach, and lets that failure settle before the
        // assertions run.
        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                let epoch = pane.session.borrow_mut().runtime.begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime.install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));
            });
        });

        cx.run_until_parked();

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                assert!(!offers(pane, "deploy"), "nothing published this yet");

                pane.on_event(SessionEvent::Commands(vec![discovered("deploy")]), cx);

                assert!(offers(pane, "deploy"), "a published command must show up");

                // Discovery is a replacement snapshot, so a later one that
                // omits the command withdraws it.
                pane.on_event(SessionEvent::Commands(vec![discovered("status")]), cx);

                assert!(!offers(pane, "deploy"), "a withdrawn command must go");
            });
        });

        cx.run_until_parked();
    }
}
