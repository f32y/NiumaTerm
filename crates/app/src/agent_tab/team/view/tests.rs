use std::path::Path;

use gpui::{AppContext as _, TestAppContext, VisualTestContext, px};
use gpui_component::Root;
use nmt_agent::AgentWorkspace;
use nmt_agent::background_task::{
    BackgroundTaskKey, BackgroundTaskRegistry, BackgroundTaskState, BackgroundTaskUpdate,
};
use nmt_agent::chat::{
    Event, Item, Question, QuestionInput, QuestionMode, QuestionRequest, SendOutcome,
    SlashCommandOutcome, ThreadSettings,
};
use nmt_agent::session::lifecycle::Status;
use nmt_agent::session::team_capabilities::{ModeratorAdmission, RecoveredTeamTurn};
use nmt_agent::session::test_support::TestBackend;
use nmt_agent::session::{AgentKind, Backend};
use nmt_agent::team::attempt::AttemptState;
use nmt_agent::team::discussion::DiscussionMode;
use nmt_agent::team::member::{MemberConfig, ProfileReference};
use nmt_agent::team::model::UserInput;
use nmt_agent::team::room::Room;
use nmt_agent::team::session::{AttemptEventKey, TeamError, TeamSession};
use nmt_config::profile::{AgentProfile, EnvVar};
use tempfile::tempdir;

use crate::agent_tab::AgentThreadDefaults;
use crate::agent_tab::execution::AgentSession;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::team::dispatch::CONTEXT_LIMITS;
use crate::agent_tab::team::{TeamCommand, TeamPane, TeamRuntime};

#[cfg(windows)]
#[gpui::test]
async fn claude_member_startup_retains_native_permission_selection(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let directory = tempdir().unwrap();

    let settings = ThreadSettings {
        model: Some("fake-claude".into()),
        approval: Some("acceptEdits".into()),
        ..ThreadSettings::default()
    };

    let (runtime, member) = cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());

        cx.set_global(AgentThreadDefaults::default());

        let runtime = TeamRuntime::create(directory.path(), AgentWorkspace::default(), cx);

        let profile = AgentProfile {
            name: "test-claude".into(),
            kind: AgentKind::Claude,
            executable: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../agent/tests/fixtures/claude/fake-stream-json.cmd")
                .to_string_lossy()
                .into_owned(),
            env: vec![EnvVar {
                name: "NMT_FAKE_STREAM_LOG".into(),
                value: directory
                    .path()
                    .join("input.jsonl")
                    .to_string_lossy()
                    .into_owned(),
            }],
            ..AgentProfile::default()
        };

        let member = runtime.update(cx, |runtime, cx| {
            runtime.add_member(
                profile,
                MemberConfig {
                    name: "Alice".into(),
                    profile: ProfileReference {
                        kind: AgentKind::Claude,
                        name: "test-claude".into(),
                    },
                    roots: AgentWorkspace::default(),
                    settings: settings.clone(),
                    role: String::new(),
                },
                cx,
            )
        });

        (runtime, member)
    });

    let member = member.await.unwrap();
    let host = cx.update(|cx| runtime.read(cx).member_session(member).unwrap().clone());

    cx.condition(&host, |session, _| {
        matches!(
            session.controller.borrow().runtime().status(),
            Status::Idle | Status::Exited
        )
    })
    .await;

    runtime.update(cx, |runtime, cx| runtime.schedule(cx));
    cx.run_until_parked();

    runtime.update(cx, |runtime, cx| {
        assert_eq!(runtime.error(), None);
        assert_eq!(
            runtime
                .member_session(member)
                .unwrap()
                .read(cx)
                .controller
                .borrow()
                .controls
                .settings,
            settings
        );
        assert_eq!(runtime.room().member(member).unwrap().settings(), &settings);
    });
}

#[gpui::test]
async fn reopened_accepted_request_shows_recovery(cx: &mut TestAppContext) {
    reopened_request(cx, false).await;
}

#[gpui::test]
async fn reopened_completed_request_recovers_exact_reply_without_resending(
    cx: &mut TestAppContext,
) {
    reopened_request(cx, true).await;
}

async fn reopened_request(cx: &mut TestAppContext, completed: bool) {
    let directory = tempdir().unwrap();

    let mut saved =
        TeamSession::create(directory.path(), Room::new(AgentWorkspace::default())).unwrap();

    let member = saved
        .add_member(MemberConfig {
            name: "Alice".into(),
            profile: ProfileReference {
                kind: AgentKind::Codex,
                name: "test".into(),
            },
            roots: AgentWorkspace::default(),
            settings: ThreadSettings::default(),
            role: "Explain clearly".into(),
        })
        .unwrap();

    saved
        .member_ready(member, 1, ModeratorAdmission::unverified(AgentKind::Codex))
        .unwrap();

    let discussion = saved
        .start_discussion(
            UserInput {
                text: "Compare the options".into(),
                ..UserInput::default()
            },
            vec![member],
            DiscussionMode::Fixed {
                report_author: member,
            },
        )
        .unwrap();

    saved
        .advance_discussion(discussion, &CONTEXT_LIMITS)
        .unwrap();

    let attempt = saved.store().room().attempts()[0].clone();

    saved
        .dispatch(attempt.id, |_| SendOutcome::StartedTurn)
        .unwrap();

    saved
        .accept_attempt(
            AttemptEventKey {
                attempt: attempt.id,
                member,
                backend_generation: 1,
            },
            "interrupted-turn",
        )
        .unwrap();

    saved
        .record_provider_identity(member, "saved-team-thread", false)
        .unwrap();

    let room_id = saved.store().room().id();

    saved.member_unavailable(member).unwrap();

    drop(saved);

    let saved = TeamSession::open(directory.path(), room_id).unwrap();

    let (runtime, pane, host, window) = cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());

        cx.set_global(AgentThreadDefaults::default());

        let runtime = cx.new(|cx| TeamRuntime::new(saved, cx.background_executor().clone()));

        let owner = AgentSession::create(
            AgentProfile {
                name: "test".into(),
                kind: AgentKind::Codex,
                ..AgentProfile::default()
            },
            AgentWorkspace::default(),
            None,
            cx,
        );

        let host = owner.session().clone();

        runtime.update(cx, |runtime, cx| {
            runtime.attach_member_owner(member, owner, cx)
        });

        let mut pane = None;

        let window = cx
            .open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| TeamPane::new(runtime.clone(), window, cx));

                pane = Some(view.clone());

                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();

        (runtime, pane.unwrap(), host, window)
    });

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    host.update(&mut cx, |session, cx| {
        let mut backend = TestBackend::new(
            [SendOutcome::StartedTurn],
            SlashCommandOutcome::NotReady,
            vec![],
        )
        .with_recovery(AgentKind::Codex, "saved-team-thread");

        backend.team_recovered_turns.push(RecoveredTeamTurn {
            id: "another-turn".into(),
            text: "Unrelated answer".into(),
        });

        if completed {
            backend.team_recovered_turns.push(RecoveredTeamTurn {
                id: "interrupted-turn".into(),
                text: "Recovered answer".into(),
            });
        }

        let epoch = session.controller.borrow_mut().starting(None);

        session.install(Ok(Backend::Test(backend)), epoch, "test", cx);

        session.on_event(epoch, Event::Ready(ThreadSettings::default()), cx);
    });

    cx.run_until_parked();

    runtime.update(&mut cx, |runtime, _| {
        assert_eq!(
            runtime.error(),
            None,
            "restored work is a recovery state, not a permission failure"
        );
        assert_eq!(
            runtime.room().attempts().len(),
            1,
            "reopening must not resend a request"
        );
    });

    pane.update(&mut cx, |pane, cx| {
        assert!(
            !pane
                .timeline
                .rows
                .iter()
                .any(|row| row.text == rust_i18n::t!("team-responding"))
        );

        if completed {
            assert!(
                pane.timeline
                    .rows
                    .iter()
                    .any(|row| row.text == "Recovered answer")
            );
        } else {
            assert!(
                !pane
                    .status_text(cx)
                    .contains(rust_i18n::t!("team-ready").as_ref())
            );

            pane.perform(TeamCommand::AbandonRestored(attempt.id), cx)
                .detach();
        }
    });

    cx.run_until_parked();

    runtime.update(&mut cx, |runtime, _| {
        assert!(runtime.pending_recovery().next().is_none());
        assert_eq!(runtime.room().attempts().len(), 1);
        assert!(if completed {
            matches!(
                runtime.room().attempts()[0].state,
                AttemptState::Completed { .. }
            )
        } else {
            runtime.room().attempts()[0].state == AttemptState::Abandoned
        });
        assert_eq!(
            runtime.room().discussions()[0].remaining_non_report_turns(runtime.room().attempts()),
            10
        );
        assert_eq!(runtime.error(), None);
    });

    assert!(
        pane.update(&mut cx, |pane, cx| pane
            .perform(TeamCommand::Continue(discussion), cx))
            .await
    );

    cx.run_until_parked();

    runtime.update(&mut cx, |runtime, _| {
        assert_eq!(runtime.error(), None);
        assert_eq!(
            runtime.room().attempts().len(),
            2,
            "Continue must start the next stage"
        );
        assert_ne!(
            runtime.room().attempts()[1].intent.operation,
            attempt.intent.operation
        );
        assert_eq!(runtime.room().attempts()[1].state, AttemptState::Sending);
    });
}

#[gpui::test]
async fn sent_team_request_displays_stream_before_completion(cx: &mut TestAppContext) {
    let directory = tempdir().unwrap();

    let (runtime, pane, host, window) = cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());

        cx.set_global(AgentThreadDefaults::default());

        let mut session =
            TeamSession::create(directory.path(), Room::new(AgentWorkspace::default())).unwrap();

        let member = session
            .add_member(MemberConfig {
                name: "Alice".into(),
                profile: ProfileReference {
                    kind: AgentKind::Codex,
                    name: "test".into(),
                },
                roots: AgentWorkspace::default(),
                settings: ThreadSettings::default(),
                role: "Explain clearly".into(),
            })
            .unwrap();

        let runtime = cx.new(|cx| TeamRuntime::new(session, cx.background_executor().clone()));

        let owner = AgentSession::create(
            AgentProfile {
                name: "test".into(),
                kind: AgentKind::Codex,
                ..AgentProfile::default()
            },
            AgentWorkspace::default(),
            None,
            cx,
        );

        let host = owner.session().clone();

        runtime.update(cx, |runtime, cx| {
            runtime.attach_member_owner(member, owner, cx)
        });

        let mut pane = None;

        let window = cx
            .open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| TeamPane::new(runtime.clone(), window, cx));

                pane = Some(view.clone());

                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();

        (runtime, pane.unwrap(), host, window)
    });

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    let epoch = host.update(&mut cx, |session, cx| {
        let backend = TestBackend::new(
            [SendOutcome::StartedTurn],
            SlashCommandOutcome::NotReady,
            vec![],
        )
        .with_recovery(AgentKind::Codex, "retained-team-thread");

        let epoch = session.controller.borrow_mut().starting(None);

        assert_eq!(
            session.install(Ok(Backend::Test(backend)), epoch, "test", cx),
            Some(true)
        );

        session.on_event(epoch, Event::Ready(ThreadSettings::default()), cx);

        epoch
    });

    cx.run_until_parked();

    runtime.update(&mut cx, |runtime, _| {
        assert_eq!(
            runtime.room().members()[0].provider_id(),
            Some("retained-team-thread")
        );
    });

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.input
                .update(cx, |input, cx| input.set_value("Say hello", window, cx));

            pane.focus(window, cx);
        })
    });

    cx.simulate_keystrokes("enter");

    cx.run_until_parked();

    runtime.update(&mut cx, |runtime, _| {
        assert_eq!(
            runtime.room().attempts().len(),
            1,
            "Enter must submit one request to the selected member"
        )
    });

    host.update(&mut cx, |session, cx| {
        session.on_event(
            epoch,
            Event::ProviderTurnAccepted {
                id: "provider-turn".into(),
            },
            cx,
        );

        session.on_event(epoch, Event::TurnStarted, cx);

        session.on_event(
            epoch,
            Event::ItemStarted(Item::AgentMessage {
                id: "reply".into(),
                text: Some(String::new()),
                questions: None,
            }),
            cx,
        );

        session.on_event(
            epoch,
            Event::AgentMessageDelta {
                item_id: "reply".into(),
                delta: "Hello from Alice".into(),
            },
            cx,
        );
    });

    cx.run_until_parked();

    pane.update(&mut cx, |pane, _| {
        assert!(
            pane.timeline
                .rows
                .iter()
                .any(|row| row.heading.contains("Alice") && row.text == "Hello from Alice"),
            "an accepted turn's streamed answer must be visible before completion"
        );
    });

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let surface = cx.debug_bounds("team-surface").unwrap();
    let messages = cx.debug_bounds("team-messages").unwrap();
    let composer = cx.debug_bounds("team-composer").unwrap();
    let reply = cx.debug_bounds("transcript-agent-1").unwrap();
    let author = cx.debug_bounds("transcript-author-1").unwrap();

    assert_eq!(
        messages.size.width, surface.size.width,
        "the conversation has no permanent sidebars"
    );
    assert!(messages.size.height > px(100.));
    assert!(messages.bottom() <= composer.top());
    assert!(composer.bottom() <= surface.bottom());
    assert!(
        reply.top() >= messages.top() && reply.bottom() <= messages.bottom(),
        "the streamed reply must be inside the message viewport"
    );
    assert!(author.size.height > px(0.));

    host.update(&mut cx, |session, cx| {
        session.on_event(
            epoch,
            Event::AgentMessageDelta {
                item_id: "reply".into(),
                delta: " again".into(),
            },
            cx,
        );
    });

    cx.run_until_parked();

    pane.update(&mut cx, |pane, cx| {
        assert!(pane.timeline.rows.iter().any(|row| row.text == "Hello from Alice again"));

        let transcript = pane.transcript.read(cx).conversation.borrow();

        assert!(transcript.content.entries().iter().any(|entry| matches!(&entry.item, Item::AgentMessage { text: Some(text), .. } if text == "Hello from Alice again")));
    });

    runtime.update(&mut cx, |runtime, _| {
        assert_eq!(
            runtime.room().messages().len(),
            1,
            "streamed text is visible without becoming completed peer context"
        )
    });

    host.update(&mut cx, |session, cx| {
        session.on_event(epoch, Event::TurnCompleted { error: None }, cx);

        session.on_event(
            epoch,
            Event::ProviderTurnFinished {
                id: "provider-turn".into(),
                error: None,
            },
            cx,
        );
    });

    cx.run_until_parked();

    pane.update(&mut cx, |pane, _| {
        assert_eq!(
            pane.timeline
                .rows
                .iter()
                .filter(|row| row.text == "Hello from Alice again")
                .count(),
            1
        );
    });

    host.update(&mut cx, |session, cx| {
        session.on_event(
            epoch,
            Event::Error {
                message: "Provider disconnected".into(),
                fatal: true,
            },
            cx,
        )
    });

    cx.run_until_parked();

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.input.update(cx, |input, cx| {
                input.set_value("Keep this draft", window, cx)
            });

            pane.focus(window, cx);
        })
    });

    cx.simulate_keystrokes("enter");

    cx.run_until_parked();

    pane.update(&mut cx, |pane, cx| {
        assert_eq!(pane.input.read(cx).text().to_string(), "Keep this draft");
        assert!(pane.error.is_some());
    });

    runtime.update(&mut cx, |runtime, _| {
        assert_eq!(runtime.room().attempts().len(), 1)
    });
}

#[gpui::test]
async fn completed_reply_waits_for_background_work_before_advancing(cx: &mut TestAppContext) {
    let directory = tempdir().unwrap();

    let (runtime, host, member, epoch) = cx.update(|cx| {
        cx.set_global(AgentSettings::default());
        cx.set_global(AgentThreadDefaults::default());

        let mut room = Room::new(AgentWorkspace::default());

        let member = room
            .add_member(MemberConfig {
                name: "Alice".into(),
                profile: ProfileReference {
                    kind: AgentKind::Codex,
                    name: "test".into(),
                },
                roots: AgentWorkspace::default(),
                settings: ThreadSettings::default(),
                role: String::new(),
            })
            .unwrap();

        let session = TeamSession::create(directory.path(), room).unwrap();
        let runtime = cx.new(|cx| TeamRuntime::new(session, cx.background_executor().clone()));

        let owner = AgentSession::create(
            AgentProfile {
                name: "test".into(),
                kind: AgentKind::Codex,
                ..AgentProfile::default()
            },
            AgentWorkspace::default(),
            None,
            cx,
        );

        let host = owner.session().clone();

        runtime.update(cx, |runtime, cx| {
            runtime.attach_member_owner(member, owner, cx)
        });

        let epoch = host.update(cx, |session, cx| {
            let backend = TestBackend::new(
                [SendOutcome::StartedTurn, SendOutcome::StartedTurn],
                SlashCommandOutcome::NotReady,
                vec![],
            )
            .with_recovery(AgentKind::Codex, "background-room");

            let epoch = session.controller.borrow_mut().starting(None);

            assert_eq!(
                session.install(Ok(Backend::Test(backend)), epoch, "test", cx),
                Some(true)
            );

            session.on_event(epoch, Event::Ready(ThreadSettings::default()), cx);

            epoch
        });

        (runtime, host, member, epoch)
    });

    cx.run_until_parked();

    runtime
        .update(cx, |runtime, cx| {
            runtime.command(
                TeamCommand::Start {
                    input: UserInput {
                        text: "Review".into(),
                        ..UserInput::default()
                    },
                    participants: vec![member],
                    mode: DiscussionMode::Fixed {
                        report_author: member,
                    },
                },
                cx,
            )
        })
        .await
        .unwrap();

    cx.run_until_parked();

    let mut tasks = BackgroundTaskRegistry::new(BackgroundTaskKey::codex("background-room"));

    let child = BackgroundTaskKey::codex("child");

    tasks.apply(
        child.clone(),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );

    host.update(cx, |session, cx| {
        session.on_event(
            epoch,
            Event::ProviderTurnAccepted { id: "first".into() },
            cx,
        );

        session.on_event(epoch, Event::TurnStarted, cx);
        session.on_event(epoch, Event::BackgroundTasks(tasks.snapshot()), cx);
        session.on_event(epoch, Event::TurnCompleted { error: None }, cx);

        session.on_event(
            epoch,
            Event::ProviderTurnFinished {
                id: "first".into(),
                error: None,
            },
            cx,
        );
    });

    cx.run_until_parked();

    let result = runtime
        .update(cx, |runtime, cx| {
            assert!(matches!(
                runtime.room().attempts()[0].state,
                AttemptState::Completed { .. }
            ));
            assert_eq!(runtime.room().attempts().len(), 1);

            runtime.command(TeamCommand::Exclude(member), cx)
        })
        .await;

    assert!(matches!(result, Err(TeamError::Busy)));

    tasks.apply(
        child,
        BackgroundTaskUpdate::state(BackgroundTaskState::Done),
    );

    host.update(cx, |session, cx| {
        session.on_event(epoch, Event::BackgroundTasks(tasks.snapshot()), cx)
    });

    cx.run_until_parked();

    runtime.update(cx, |runtime, _| {
        assert_eq!(runtime.room().attempts().len(), 2);
        assert_eq!(runtime.room().attempts()[1].state, AttemptState::Sending);
        assert_eq!(runtime.error(), None);
    });
}

#[gpui::test]
async fn member_question_shows_in_team_composer_and_blocks_new_requests(cx: &mut TestAppContext) {
    let directory = tempdir().unwrap();

    let (runtime, pane, host, window, member) = cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());

        cx.set_global(AgentThreadDefaults::default());

        let mut session =
            TeamSession::create(directory.path(), Room::new(AgentWorkspace::default())).unwrap();

        let member = session
            .add_member(MemberConfig {
                name: "Alice".into(),
                profile: ProfileReference {
                    kind: AgentKind::Codex,
                    name: "test".into(),
                },
                roots: AgentWorkspace::default(),
                settings: ThreadSettings::default(),
                role: "Explain clearly".into(),
            })
            .unwrap();

        let runtime = cx.new(|cx| TeamRuntime::new(session, cx.background_executor().clone()));

        let owner = AgentSession::create(
            AgentProfile {
                name: "test".into(),
                kind: AgentKind::Codex,
                ..AgentProfile::default()
            },
            AgentWorkspace::default(),
            None,
            cx,
        );

        let host = owner.session().clone();

        runtime.update(cx, |runtime, cx| {
            runtime.attach_member_owner(member, owner, cx)
        });

        let mut pane = None;

        let window = cx
            .open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| TeamPane::new(runtime.clone(), window, cx));

                pane = Some(view.clone());

                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();

        (runtime, pane.unwrap(), host, window, member)
    });

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    host.update(&mut cx, |session, cx| {
        let backend = TestBackend::new(
            [SendOutcome::StartedTurn],
            SlashCommandOutcome::NotReady,
            vec![],
        )
        .with_recovery(AgentKind::Codex, "asking-team-thread");

        let epoch = session.controller.borrow_mut().starting(None);

        session.install(Ok(Backend::Test(backend)), epoch, "test", cx);

        session.on_event(epoch, Event::Ready(ThreadSettings::default()), cx);

        session.on_event(
            epoch,
            Event::InputRequested(QuestionRequest {
                id: "scope".into(),
                mode: QuestionMode::Blocking,
                questions: vec![Question {
                    input: QuestionInput::Text,
                    header: None,
                    question: "How far should this go?".into(),
                    multi_select: false,
                    options: Vec::new(),
                }],
            }),
            cx,
        );
    });

    cx.run_until_parked();

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let composer = cx.debug_bounds("team-composer").unwrap();

    let question = cx
        .debug_bounds("agent-question-panel")
        .expect("a member's question must be drawn in the Team composer");

    assert!(
        question.top() >= composer.top() && question.bottom() <= composer.bottom(),
        "the question belongs inside the composer card"
    );

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.input.update(cx, |input, cx| {
                input.set_value("Go all the way", window, cx)
            });

            pane.focus(window, cx);
        })
    });

    cx.simulate_keystrokes("enter");

    cx.run_until_parked();

    runtime.update(&mut cx, |runtime, _| {
        assert!(
            runtime.room().attempts().is_empty(),
            "a request to a member waiting on the user must not be queued"
        );
    });

    pane.update(&mut cx, |pane, cx| {
        assert_eq!(
            pane.error.as_deref(),
            Some(rust_i18n::t!("team-member-needs-answer", name = "Alice").as_ref())
        );
        assert_eq!(pane.input.read(cx).text().to_string(), "Go all the way");
    });

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| pane.inspect_member(member, window, cx));

        let _ = window.draw(cx);
    });

    let surface = cx.debug_bounds("team-surface").unwrap();

    let composer = cx
        .debug_bounds("team-member-composer")
        .expect("the member's view carries its own composer card");

    let question = cx.debug_bounds("agent-question-panel").unwrap();

    assert!(
        composer.size.width < surface.size.width,
        "the member's card keeps the reading column instead of the pane width"
    );
    assert!(
        question.left() >= composer.left() && question.right() <= composer.right(),
        "the question sits inside the member's card"
    );
}
