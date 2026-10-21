use nmt_agent::background_task::{
    BackgroundTaskKey, BackgroundTaskLoadState, BackgroundTaskRegistry, BackgroundTaskState,
    BackgroundTaskUpdate,
};

use crate::agent_tab::session::directories_match;

#[test]
fn a_failed_refresh_reports_unavailable_without_dropping_known_rows() {
    let parent = BackgroundTaskKey::codex("thread-a");

    let mut registry = BackgroundTaskRegistry::new(parent);

    registry.apply(
        BackgroundTaskKey::codex("child-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );

    registry.set_discovery(BackgroundTaskLoadState::Unavailable {
        message: "thread/list failed".into(),
    });

    let snapshot = registry.snapshot();

    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.active_count(), 1);
    assert!(matches!(
        snapshot.discovery,
        BackgroundTaskLoadState::Unavailable { .. }
    ));
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

mod conversation_title_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{
        AppContext as _, Entity, Subscription, TestAppContext, VisualTestContext, WindowHandle,
    };
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{SendOutcome, SlashCommandOutcome};
    use nmt_agent::session::lifecycle::StartOutcome;
    use nmt_config::profile::{AgentKind, AgentProfile};

    use crate::agent_tab::session::{Backend, RecoveryIdentity, Status, TestBackend};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::tests::deliver_session_event;
    use crate::agent_tab::{AgentPane, AgentPaneEvent};

    fn open_pane(
        cx: &mut TestAppContext,
        kind: AgentKind,
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
        let (pane, _) = open_pane(cx, AgentKind::Codex, None);

        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                let initial = pane.command_catalog(cx);

                assert!(Rc::ptr_eq(&initial, &pane.command_catalog(cx)));

                pane.palette.catalog.as_mut().unwrap().language = "previous-language".into();

                let refreshed = pane.command_catalog(cx);

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
    fn discovery_cache_drops_commands_and_skills_from_a_retired_session(cx: &mut TestAppContext) {
        use nmt_agent::chat::{
            Event, SkillCatalog, SlashCommandArguments, SlashCommandInfo, SlashCommandRunPolicy,
            SlashCommandSource,
        };

        let (pane, _) = open_pane(cx, AgentKind::Codex, None);

        deliver_session_event(
            &pane,
            Event::Commands(vec![SlashCommandInfo {
                name: "old-provider-command".into(),
                description: "A command from the previous session".into(),
                argument_hint: None,
                source: SlashCommandSource::Provider,
                arguments: SlashCommandArguments::None,
                run_policy: SlashCommandRunPolicy::Immediate,
            }]),
            cx,
        );

        deliver_session_event(
            &pane,
            Event::Skills(SkillCatalog {
                skills: Vec::new(),
                errors: vec!["previous discovery error".into()],
            }),
            cx,
        );

        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                let before = pane.command_catalog(cx);

                assert!(
                    before
                        .iter()
                        .any(|command| command.name == "old-provider-command")
                );
                assert_eq!(
                    pane.skill_palette_model("").note.as_deref(),
                    Some("previous discovery error")
                );

                pane.session.borrow_mut().starting(None);

                let after = pane.command_catalog(cx);

                assert!(
                    !after
                        .iter()
                        .any(|command| command.name == "old-provider-command")
                );
                assert!(!Rc::ptr_eq(&before, &after));
                assert_eq!(
                    pane.skill_palette_model("").note.as_deref(),
                    Some(&*rust_i18n::t!("agent-composer-skill-discovery-loading"))
                );
            })
        });
    }

    #[gpui::test]
    fn output_failure_retires_backend_and_marks_session_exited(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx, AgentKind::Codex, None);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [],
                            SlashCommandOutcome::NotReady,
                            vec![],
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime_mut().turn_started();

                pane.stop_for_output_failure("Output limit reached".into(), cx);

                assert!(pane.session.borrow().runtime().backend().is_none());
                assert_eq!(pane.session.borrow().runtime().status(), Status::Exited);
            });
        });
    }

    #[gpui::test]
    fn rejected_rename_keeps_latest_name_until_admitted(cx: &mut TestAppContext) {
        use nmt_agent::session::RenameOutcome;

        use crate::agent_tab::profile::AgentKind;

        let (pane, window) = open_pane(cx, AgentKind::Codex, None);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                let mut backend = TestBackend::new([], SlashCommandOutcome::NotReady, vec![])
                    .with_recovery(AgentKind::Codex, "thread");

                backend.rename_outcome = RenameOutcome::Rejected;

                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session
                        .borrow_mut()
                        .runtime_mut()
                        .install(epoch, Ok(Backend::Test(backend))),
                    StartOutcome::Installed
                ));

                pane.rename_session("first");

                pane.rename_session("latest");

                assert_eq!(
                    pane.session.borrow_mut().naming_mut().pending.as_deref(),
                    Some("latest")
                );

                pane.session.borrow_mut().sync_pending_rename();

                assert_eq!(
                    pane.session.borrow_mut().naming_mut().pending.as_deref(),
                    Some("latest")
                );

                let mut state = pane.session.borrow_mut();

                let Some(Backend::Test(backend)) = state.runtime_mut().backend_mut() else {
                    panic!("expected test backend");
                };

                backend.rename_outcome = RenameOutcome::Accepted;

                drop(state);

                pane.session.borrow_mut().sync_pending_rename();

                assert!(pane.session.borrow_mut().naming_mut().pending.is_none());

                let mut state = pane.session.borrow_mut();

                let Some(Backend::Test(backend)) = state.runtime_mut().backend_mut() else {
                    panic!("expected test backend");
                };

                backend.rename_outcome = RenameOutcome::Unsupported;

                drop(state);

                pane.rename_session("local only");

                assert!(pane.session.borrow_mut().naming_mut().pending.is_none());
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

        let (pane, window) = open_pane(cx, AgentKind::Codex, None);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().restore_questions();
            })
        });

        deliver_session_event(
            &pane,
            Event::ApprovalRequested {
                description: "Run a command".into(),
            },
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                pane.respond_approval("accept", cx);

                assert!(pane.session.borrow().input().approval().is_some());
            })
        });

        deliver_session_event(&pane, Event::ApprovalResolved, &cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                pane.session.borrow_mut().runtime_mut().ready();

                pane.restore_question_drafts();

                if let Some(Backend::Test(backend)) =
                    pane.session.borrow_mut().runtime_mut().backend_mut()
                {
                    backend.input_result = Err("The question response could not be queued.".into());
                }
            })
        });

        deliver_session_event(
            &pane,
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
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                pane.skip_current_questions(cx);

                let state = pane.session.borrow();

                let question = pane
                    .prompts
                    .questions(state.input())
                    .expect("rejected answer remains visible");

                assert!(question.error().is_some());
            })
        });
    }

    #[gpui::test]
    fn accepted_codex_prompt_publishes_a_provisional_title(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx, AgentKind::Codex, None);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        let (titles, _subscription) = collect_titles(&pane, &mut cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime_mut().ready();

                assert!(pane.send_text_inner(
                    "  Inspect title generation\n and its fallback  ".into(),
                    None,
                    None,
                    cx
                ));
                assert!(pane.session.borrow_mut().naming_mut().named);
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
        let (pane, window) = open_pane(cx, AgentKind::Codex, None);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        let (titles, _subscription) = collect_titles(&pane, &mut cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                pane.host
                    .upgrade()
                    .unwrap()
                    .update(cx, |host, _| host.kind = AgentKind::Claude);

                pane.session.borrow_mut().set_kind(AgentKind::Claude);

                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn, SendOutcome::Steered],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime_mut().ready();

                assert!(pane.send_text_inner(
                    "one two three four five six seven eight".into(),
                    None,
                    None,
                    cx
                ));
                assert!(pane.session.borrow_mut().naming_mut().named);
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
            AgentKind::Codex,
            Some(RecoveryIdentity::new(
                AgentKind::Codex,
                "resumed-conversation",
            )),
        );

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        let (titles, _subscription) = collect_titles(&pane, &mut cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                assert!(pane.session.borrow_mut().naming_mut().named);

                pane.host
                    .upgrade()
                    .unwrap()
                    .update(cx, |host, _| host.kind = AgentKind::Claude);

                pane.session.borrow_mut().set_kind(AgentKind::Claude);

                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime_mut().ready();

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
        Event as SessionEvent, Item as SessionItem, PromptPlacement, QueuedPrompt, SendOutcome,
        SlashCommandOutcome,
    };
    use nmt_agent::session::lifecycle::StartOutcome;
    use nmt_config::profile::{AgentKind, AgentProfile};

    use crate::agent_tab::session::{Backend, Status, TestBackend};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::tests::deliver_session_event;
    use crate::agent_tab::{AgentPane, input_history};

    fn open_pane(
        cx: &mut TestAppContext,
        kind: AgentKind,
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

    /// Every user row as the turn it was filed under and the text it holds.
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
        let (pane, window) = open_pane(cx, AgentKind::Codex);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        let previous_turn = cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime_mut().ready();

                pane.session.borrow_mut().commands_mut().awaiting_turn = true;

                let previous_turn = pane.session.borrow().turn();

                assert!(!pane.transcript.read(cx).is_working());

                previous_turn
            })
        });

        deliver_session_event(&pane, SessionEvent::TurnStarted, &cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                assert!(!pane.session.borrow_mut().commands_mut().awaiting_turn);
                assert_eq!(pane.session.borrow().turn(), previous_turn + 1);
                assert_eq!(pane.session.borrow().runtime().status(), Status::Running);
                assert!(pane.transcript.read(cx).is_working());
            })
        });

        deliver_session_event(&pane, SessionEvent::TurnStarted, &cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                assert_eq!(
                    pane.session.borrow().turn(),
                    previous_turn + 1,
                    "a repeated event must not open another turn"
                );
            })
        });
    }

    #[gpui::test]
    fn a_queued_prompt_heads_the_turn_opened_for_it(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx, AgentKind::Claude);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn, SendOutcome::Steered],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime_mut().ready();

                assert!(pane.send_text_inner("open the turn".into(), None, None, cx));
            })
        });

        deliver_session_event(&pane, SessionEvent::TurnStarted, &cx);

        let first_turn = cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let first_turn = pane.session.borrow().turn();

                assert!(pane.send_text_inner("queued behind it".into(), None, None, cx));

                first_turn
            })
        });

        deliver_session_event(
            &pane,
            SessionEvent::ItemStarted(SessionItem::AgentMessage {
                id: "msg-1".into(),
                text: Some("the first answer".into()),
                questions: None,
            }),
            &cx,
        );

        deliver_session_event(&pane, SessionEvent::TurnCompleted { error: None }, &cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                assert_eq!(
                    user_rows(pane, cx),
                    vec![(first_turn, "open the turn".to_string())],
                    "the finished turn keeps only the prompt that opened it"
                );
            })
        });

        // The CLI answers the held prompt in a turn nothing here sent.
        deliver_session_event(&pane, SessionEvent::TurnStarted, &cx);

        deliver_session_event(
            &pane,
            SessionEvent::ItemStarted(SessionItem::UserMessage {
                text: Some("queued behind it".into()),
            }),
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                assert_eq!(
                    pane.session.borrow().turn(),
                    first_turn + 1,
                    "that turn is numbered"
                );
                assert_eq!(pane.session.borrow().runtime().status(), Status::Running);
                assert!(pane.transcript.read(cx).is_working());
                assert_eq!(
                    user_rows(pane, cx),
                    vec![
                        (first_turn, "open the turn".to_string()),
                        (first_turn + 1, "queued behind it".to_string()),
                    ],
                    "the held prompt heads the turn that answers it"
                );
            })
        });
    }

    /// The harness lists a prompt in its pending inbox from the moment it
    /// accepts it until the turn claims it, and the send that started that
    /// turn has already drawn the prompt's row. Listing it above the composer
    /// as well would show the same message twice for that whole window.
    #[gpui::test]
    fn a_pending_inbox_omits_the_prompt_its_send_already_drew(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx, AgentKind::DeepSeek);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        let text = cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime_mut().ready();

                let text = "Reply with exactly: ok".to_string();

                assert!(pane.send_text_inner(text.clone(), None, None, cx));

                text
            })
        });

        // What the harness reports, in the order it reports it: the
        // prompt queued, the turn opened, the queue emptied, and the
        // harness's own echo of the message it took.

        deliver_session_event(
            &pane,
            SessionEvent::QueuedPrompts(vec![QueuedPrompt {
                id: Some("afd4d197".into()),
                text: text.clone(),
                placement: PromptPlacement::Queued,
            }]),
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                assert!(
                    pane.session.borrow().queued_prompts().is_empty(),
                    "a prompt already in the transcript is not also waiting"
                );
            })
        });

        deliver_session_event(&pane, SessionEvent::TurnStarted, &cx);

        deliver_session_event(&pane, SessionEvent::QueuedPrompts(Vec::new()), &cx);

        deliver_session_event(
            &pane,
            SessionEvent::ItemStarted(SessionItem::UserMessage {
                text: Some(text.clone()),
            }),
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                assert_eq!(
                    user_rows(pane, cx),
                    vec![(1, text)],
                    "the message appears once, in the turn it opened"
                );
                assert!(pane.session.borrow().queued_prompts().is_empty());
            })
        });
    }

    #[gpui::test]
    fn a_correction_control_appears_only_while_a_turn_runs_with_text_to_send(
        cx: &mut TestAppContext,
    ) {
        let (pane, window) = open_pane(cx, AgentKind::Codex);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime_mut().ready();
            });
        });

        let draw = |cx: &mut VisualTestContext| {
            cx.run_until_parked();

            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        };

        // Idle with an empty composer: the trailing button is the only control.
        draw(&mut cx);

        assert!(cx.debug_bounds("agent-steer").is_none());

        // Text written while idle is a message like any other, and the trailing
        // button already sends it.
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.input.update(cx, |input, cx| {
                    input.set_value("stop at the parser", window, cx)
                });
            });
        });

        draw(&mut cx);

        assert!(cx.debug_bounds("agent-steer").is_none());

        // The same text while a turn runs is a correction, and the trailing
        // button stops that turn instead of sending, so the correction needs a
        // control of its own.
        deliver_session_event(&pane, SessionEvent::TurnStarted, &cx);

        draw(&mut cx);

        assert!(cx.debug_bounds("agent-steer").is_some());

        // It goes away again once there is nothing to correct.
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
            });
        });

        draw(&mut cx);

        assert!(cx.debug_bounds("agent-steer").is_none());
    }

    #[gpui::test]
    fn clicking_the_correction_control_sends_the_message_into_the_running_turn(
        cx: &mut TestAppContext,
    ) {
        let (pane, window) = open_pane(cx, AgentKind::Codex);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        // Sending records what the composer held. The application installs that
        // store at startup, and a window opened by a test has to do the same.
        cx.update(|_, app| input_history::initialize(true, app));

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::Steered],
                            SlashCommandOutcome::NotReady,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));

                pane.session.borrow_mut().runtime_mut().ready();
            });
        });

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.input.update(cx, |input, cx| {
                    input.set_value("stop at the parser", window, cx)
                });
            });
        });

        deliver_session_event(&pane, SessionEvent::TurnStarted, &cx);

        cx.run_until_parked();

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let correction = cx
            .debug_bounds("agent-steer")
            .expect("the correction control is drawn while a turn runs");

        cx.simulate_click(correction.center(), Default::default());

        cx.run_until_parked();

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let waiting = pane.session.borrow().queued_prompts().clone();

                assert_eq!(waiting.len(), 1, "the correction waits on the running turn");
                assert_eq!(waiting[0].text, "stop at the parser");
                assert_eq!(
                    waiting[0].placement,
                    PromptPlacement::Steering,
                    "the model reads it at this turn's next step"
                );
                assert_eq!(
                    pane.input.read(cx).text().len(),
                    0,
                    "the correction left the composer"
                );
            });
        });
    }
}

mod turn_error_tests {
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{Event as SessionEvent, Item as SessionItem};
    use nmt_config::profile::{AgentKind, AgentProfile};

    use crate::agent_tab::AgentPane;
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::tests::deliver_session_event;

    fn open_pane(
        cx: &mut TestAppContext,
    ) -> (Entity<AgentPane>, WindowHandle<gpui_component::Root>) {
        let profile = AgentProfile {
            name: "Turn Error Test".into(),
            kind: AgentKind::Codex,
            executable: "missing-codex.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);

            cx.set_global(AgentSettings::default());

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

        deliver_session_event(&pane, SessionEvent::TurnStarted, &cx);

        deliver_session_event(
            &pane,
            SessionEvent::ItemStarted(SessionItem::AgentMessage {
                id: "message".into(),
                text: Some("partial answer".into()),
                questions: None,
            }),
            &cx,
        );

        deliver_session_event(
            &pane,
            SessionEvent::Error {
                message: "model unavailable".into(),
                fatal: false,
            },
            &cx,
        );

        deliver_session_event(
            &pane,
            SessionEvent::TurnCompleted {
                error: Some("model unavailable".into()),
            },
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
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
            })
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
    use nmt_config::profile::{AgentKind, AgentProfile};

    use crate::agent_tab::AgentPane;
    use crate::agent_tab::session::{Backend, TestBackend};
    use crate::agent_tab::settings::AgentSettings;

    #[gpui::test]
    fn a_reset_holds_its_old_session_until_the_replacement_is_installed(cx: &mut TestAppContext) {
        let profile = AgentProfile {
            name: "Session Replacement Test".into(),
            kind: AgentKind::DeepSeek,
            // The replacement start never reaches a process: the spawn runs on
            // the background executor, which this test does not run.
            executable: "missing-agent.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);

            cx.set_global(AgentSettings::default());

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
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
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

                pane.session.borrow_mut().runtime_mut().ready();

                pane.reset_conversation(cx);

                assert!(
                    pane.session.borrow().runtime().backend().is_none(),
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
    use nmt_config::profile::{AgentKind, AgentProfile};

    use crate::agent_tab::AgentPane;
    use crate::agent_tab::session::{Backend, Status, TestBackend, UpdateSuspension};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::tests::deliver_session_event;

    #[gpui::test]
    fn a_host_exit_retains_the_thread_for_retry(cx: &mut TestAppContext) {
        let profile = AgentProfile {
            name: "Codex Recovery Test".into(),
            kind: AgentKind::Codex,
            executable: "missing-codex.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);

            cx.set_global(AgentSettings::default());

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
            pane.update(cx, |pane, _| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
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

                pane.session.borrow_mut().runtime_mut().ready();
            })
        });

        deliver_session_event(
            &pane,
            SessionEvent::HostExited {
                message: "Codex app-server stopped unexpectedly".into(),
            },
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                assert_eq!(pane.session.borrow().runtime().status(), Status::Exited);
                assert!(matches!(
                    pane.session.borrow().runtime().update_suspension(),
                    Some(UpdateSuspension::Failed(_))
                ));

                let state = pane.session.borrow();

                let snapshot = state
                    .runtime()
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
                assert!(pane.session.borrow().runtime().backend().is_some());
            })
        });
    }
}

/// The merged `/` catalog is held between frames instead of rebuilt on each
/// one, so the two ways of getting it wrong are keeping a command the harness
/// has withdrawn and never showing one it has just published.
mod command_catalog_cache_tests {
    use gpui::{App, AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{
        Event as SessionEvent, SendOutcome, SlashCommandArguments, SlashCommandInfo,
        SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
    };
    use nmt_agent::session::lifecycle::StartOutcome;
    use nmt_config::profile::{AgentKind, AgentProfile};

    use crate::agent_tab::AgentPane;
    use crate::agent_tab::session::{Backend, TestBackend};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::tests::deliver_session_event;

    fn open_pane(
        cx: &mut TestAppContext,
    ) -> (Entity<AgentPane>, WindowHandle<gpui_component::Root>) {
        let profile = AgentProfile {
            name: "Catalog Cache Test".into(),
            kind: AgentKind::Codex,
            // Never spawned: the test publishes discovery results by hand.
            executable: "missing-agent.exe".into(),
            ..AgentProfile::default()
        };

        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);

            cx.set_global(AgentSettings::default());

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

    fn offers(pane: &mut AgentPane, name: &str, cx: &App) -> bool {
        pane.command_catalog(cx)
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
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
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
                assert!(!offers(pane, "deploy", cx), "nothing published this yet");
            })
        });

        deliver_session_event(
            &pane,
            SessionEvent::Commands(vec![discovered("deploy")]),
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                assert!(
                    offers(pane, "deploy", cx),
                    "a published command must show up"
                );
            })
        });

        // Discovery is a replacement snapshot, so a later one that
        // omits the command withdraws it.
        deliver_session_event(
            &pane,
            SessionEvent::Commands(vec![discovered("status")]),
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                assert!(!offers(pane, "deploy", cx), "a withdrawn command must go");
            })
        });

        cx.run_until_parked();
    }
}

/// A failed start set aside for a blank tab leaves the harness down until
/// the user sends something, and what was typed goes out once the harness
/// launched for it is ready.
mod failed_start_tests {
    use std::path::PathBuf;
    use std::time::SystemTime;
    use std::{env, fs, process};

    use gpui::{
        AppContext as _, Entity, SharedString, TestAppContext, VisualTestContext, WindowHandle,
    };
    use nmt_agent::AgentWorkspace;
    use nmt_agent::chat::{
        Event, SendOutcome, SessionSummary, SlashCommandOutcome, ThreadSettings,
    };
    use nmt_agent::input_history::AgentInputHistory as InputHistoryService;
    use nmt_agent::session::lifecycle::{StartOutcome, Status};
    use nmt_config::profile::{AgentKind, AgentProfile};

    use crate::agent_tab::input_history::AgentInputHistory;
    use crate::agent_tab::session::{Backend, TestBackend};
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::tests::deliver_session_event;
    use crate::agent_tab::{AgentPane, RecentSessionsMode};

    fn history_path() -> PathBuf {
        env::temp_dir().join(format!("nmt-failed-start-history-{}.json", process::id()))
    }

    fn open_pane(
        cx: &mut TestAppContext,
    ) -> (Entity<AgentPane>, WindowHandle<gpui_component::Root>) {
        open_pane_on(
            cx,
            AgentProfile {
                name: "Failed Start Test".into(),
                kind: AgentKind::Codex,
                // Every launch is superseded by one the test installs by hand.
                executable: "missing-agent.exe".into(),
                ..AgentProfile::default()
            },
        )
    }

    fn open_pane_on(
        cx: &mut TestAppContext,
        profile: AgentProfile,
    ) -> (Entity<AgentPane>, WindowHandle<gpui_component::Root>) {
        let mut pane = None;

        let window = cx.update(|cx| {
            gpui_component::init(cx);

            cx.set_global(AgentSettings::default());

            // A sent message is recorded in the input history.
            cx.set_global(AgentInputHistory(InputHistoryService::open(history_path())));

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
    fn blank_tab_after_failed_start_launches_on_send(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                pane.install_started_session(Err("codex missing".into()), epoch, "Codex", cx);

                assert!(!pane.transcript.read(cx).is_empty(), "failure row shown");

                pane.return_to_blank_tab(cx);
            });
        });

        cx.run_until_parked();

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                assert!(pane.transcript.read(cx).is_empty());
                assert!(pane.history_ui.mode == RecentSessionsMode::Automatic);
                assert!(pane.launch_deferred());

                pane.input
                    .update(cx, |input, cx| input.set_value("hello", window, cx));

                pane.send_user_message_now(window, cx);

                assert_eq!(pane.session.borrow().runtime().status(), Status::Starting);
                assert!(pane.send_on_ready);
                assert!(!pane.shows_start_overlay(), "held message shows in place");
                assert_eq!(pane.input.read(cx).text().len(), 0, "message left composer");
                assert_eq!(
                    pane.held_draft.as_ref().map(|draft| draft.text.as_str()),
                    Some("hello")
                );

                // Stands in for the launch the send asked for.
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                assert!(matches!(
                    pane.session.borrow_mut().runtime_mut().install(
                        epoch,
                        Ok(Backend::Test(TestBackend::new(
                            [SendOutcome::StartedTurn],
                            SlashCommandOutcome::Accepted,
                            Vec::new(),
                        )))
                    ),
                    StartOutcome::Installed
                ));
            });
        });

        deliver_session_event(&pane, Event::Ready(ThreadSettings::default()), &cx);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.send_held_input(window, cx);

                assert!(!pane.send_on_ready);
                assert!(pane.held_draft.is_none());
                assert_eq!(pane.input.read(cx).text().len(), 0, "held input sent");
            });
        });

        cx.run_until_parked();

        let history = history_path();

        let _ = fs::remove_file(history.with_extension("json.lock"));
        let _ = fs::remove_file(history);
    }

    #[gpui::test]
    fn resume_before_the_session_list_arrives_opens_it_when_it_does(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.input
                    .update(cx, |input, cx| input.set_value("/resume", window, cx));

                pane.history_ui.data.begin_filesystem_history(None);

                pane.send_user_message_now(window, cx);

                // The handshake is delivering its list, so the command waits
                // instead of reporting the folder empty.
                assert_eq!(pane.history_ui.mode, RecentSessionsMode::AwaitingRows);

                let expected =
                    SharedString::from(rust_i18n::t!("agent-composer-listing-recent-sessions"));

                assert_eq!(
                    pane.palette
                        .feedback
                        .as_ref()
                        .map(|feedback| &feedback.message),
                    Some(&expected)
                );
            });
        });

        deliver_session_event(
            &pane,
            Event::History(vec![SessionSummary {
                id: "listed".into(),
                title: "listed".into(),
                branch: None,
                cwd: None,
                last_active: SystemTime::UNIX_EPOCH,
                snippet: None,
                origin: None,
            }]),
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, _| {
                // The list opens for the command that asked for it, and the
                // notice about listing stands down.
                assert_eq!(pane.history_ui.mode, RecentSessionsMode::Open);
                assert!(pane.palette.feedback.is_none());
                assert!(pane.history_ui.is_visible(false, true));
            });
        });
    }

    #[gpui::test]
    fn deferred_tab_shows_no_start_until_input(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                // Stands in for a tab created without a launch: a start epoch
                // that no harness answers.
                pane.session.borrow_mut().runtime_mut().begin_start();

                pane.defer_launch(cx);

                assert!(pane.launch_deferred());
                assert!(!pane.shows_start_overlay());

                pane.input
                    .update(cx, |input, cx| input.set_value("/status", window, cx));

                pane.send_user_message_now(window, cx);

                assert!(!pane.launch_deferred());
                assert!(!pane.shows_start_overlay());
                assert!(pane.send_on_ready);
                assert!(pane.held_draft.is_none(), "a command stays in the composer");
                assert_eq!(pane.input.read(cx).text().to_string(), "/status");
            });
        });
    }

    #[gpui::test]
    fn failed_launch_returns_held_message_to_composer(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.session.borrow_mut().runtime_mut().begin_start();

                pane.defer_launch(cx);

                pane.input
                    .update(cx, |input, cx| input.set_value("hello", window, cx));

                pane.send_user_message_now(window, cx);

                assert!(pane.held_draft.is_some());

                // A second submit while the first still waits is not a
                // second message.
                pane.send_user_message_now(window, cx);

                assert!(pane.send_on_ready);

                // Stands in for the launch the send asked for, failing.
                let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

                pane.install_started_session(Err("codex missing".into()), epoch, "Codex", cx);

                pane.send_held_input(window, cx);

                assert!(!pane.send_on_ready);
                assert!(pane.held_draft.is_none());
                assert_eq!(pane.input.read(cx).text().to_string(), "hello");
            });
        });
    }

    #[gpui::test]
    fn deferred_tab_shows_last_reported_controls(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        deliver_session_event(
            &pane,
            Event::Ready(ThreadSettings {
                model: Some("reported-model".into()),
                effort: Some("high".into()),
                ..ThreadSettings::default()
            }),
            &cx,
        );

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                // A tab on the same profile that never launched starts with
                // empty controls.
                pane.session.borrow_mut().runtime_mut().begin_start();

                pane.session.borrow_mut().controls.settings = ThreadSettings::default();

                pane.defer_launch(cx);

                let session = pane.session.borrow();

                assert_eq!(
                    session.controls.settings.model.as_deref(),
                    Some("reported-model")
                );
                assert_eq!(session.controls.settings.effort.as_deref(), Some("high"));
            });
        });
    }

    #[gpui::test]
    fn deferred_tab_without_report_shows_profile_pins(cx: &mut TestAppContext) {
        let (pane, window) = open_pane_on(
            cx,
            AgentProfile {
                name: "Never Launched Test".into(),
                kind: AgentKind::Codex,
                executable: "missing-agent.exe".into(),
                model: "pinned-model".into(),
                effort: "high".into(),
                ..AgentProfile::default()
            },
        );

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                pane.session.borrow_mut().runtime_mut().begin_start();

                pane.session.borrow_mut().controls.settings = ThreadSettings::default();

                pane.defer_launch(cx);

                let session = pane.session.borrow();

                assert_eq!(
                    session.controls.settings.model.as_deref(),
                    Some("pinned-model")
                );
                assert_eq!(session.controls.settings.effort.as_deref(), Some("high"));
            });
        });
    }

    #[gpui::test]
    fn disk_history_completes_resume_wait_without_a_protocol_event(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let request = pane.history_ui.data.begin_filesystem_history(None);

                pane.history_ui.await_listing();
                pane.history_ui.publish_filesystem_count(&request, None, 1);

                pane.history_ui.publish_filesystem_rows(
                    &request,
                    None,
                    vec![SessionSummary {
                        id: "disk".into(),
                        title: "disk".into(),
                        branch: None,
                        cwd: None,
                        last_active: SystemTime::UNIX_EPOCH,
                        snippet: None,
                        origin: None,
                    }],
                );

                pane.finish_history_listing(cx);

                assert_eq!(pane.history_ui.mode, RecentSessionsMode::Open);
                assert!(pane.palette.feedback.is_none());
            });
        });
    }

    #[gpui::test]
    fn resume_history_waits_for_remaining_sources_after_an_empty_result(cx: &mut TestAppContext) {
        let (pane, window) = open_pane(cx);

        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                let request = pane.history_ui.data.begin_other_agents(2);

                pane.history_ui.await_listing();
                pane.history_ui.data.finish_other_agent(&request);
                pane.on_history(Vec::new(), cx);

                assert_eq!(pane.history_ui.mode, RecentSessionsMode::AwaitingRows);

                pane.history_ui.data.finish_other_agent(&request);
                pane.finish_history_listing(cx);

                assert_eq!(pane.history_ui.mode, RecentSessionsMode::Hidden);

                let expected =
                    SharedString::from(rust_i18n::t!("agent-composer-no-recent-sessions"));

                assert_eq!(
                    pane.palette
                        .feedback
                        .as_ref()
                        .map(|feedback| &feedback.message),
                    Some(&expected)
                );
                assert!(!pane.history_ui.data.list_in_flight());
            });
        });
    }
}
