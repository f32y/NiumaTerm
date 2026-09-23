use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;

use gpui::{
    App, AppContext as _, Entity, Subscription, TestAppContext, VisualTestContext, WindowHandle,
};
use gpui_component::Root;
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::{
    Event, Item, ReplayItem, ReplayTurn, SessionSummary, SlashCommandOutcome, ThreadSettings,
};
use nmt_agent::session::lifecycle::{StartOutcome, Status};
use nmt_agent::session::restore::{
    LoadedReplay, ReadyAction, ReplayLoaded, ResumeStart, SettingsSeed,
};
use nmt_agent::session::{AgentKind, RecoveryIdentity, ResumeOutcome};
use nmt_config::profile::AgentProfile;

use crate::agent_tab::session::{Backend, TestBackend};
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::tests::deliver_session_event;
use crate::agent_tab::{AgentPane, AgentPaneEvent, RecentSessionsMode};

fn open_pane(cx: &mut TestAppContext) -> (Entity<AgentPane>, WindowHandle<Root>) {
    let profile = AgentProfile {
        name: "Restore Test".into(),
        kind: AgentKind::Codex,
        // The test installs a backend before the initial async start is polled.
        executable: "missing-agent.exe".into(),
        ..AgentProfile::default()
    };

    let mut pane = None;

    let window = cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());

        cx.open_window(Default::default(), |window, cx| {
            let agent = cx.new(|cx| AgentPane::new(profile, AgentWorkspace::default(), window, cx));

            pane = Some(agent.clone());

            cx.new(|cx| Root::new(agent, window, cx))
        })
        .expect("open restore test window")
    });

    (pane.expect("create pane"), window)
}

fn summary() -> SessionSummary {
    SessionSummary {
        id: "selected".into(),
        title: "Selected".into(),
        branch: None,
        cwd: None,
        last_active: SystemTime::UNIX_EPOCH,
        snippet: None,
    }
}

fn replay(text: &str) -> Vec<ReplayTurn> {
    vec![ReplayTurn {
        items: vec![ReplayItem {
            item: Item::UserMessage {
                text: Some(text.into()),
            },
            at: None,
        }],
        ..ReplayTurn::default()
    }]
}

fn user_rows(pane: &AgentPane, cx: &App) -> Vec<String> {
    pane.transcript
        .read(cx)
        .conversation
        .borrow()
        .content
        .entries()
        .iter()
        .filter_map(|entry| match &entry.item {
            Item::UserMessage { text } => text.clone(),
            _ => None,
        })
        .collect()
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

fn install_backend(pane: &mut AgentPane) {
    let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

    pane.session
        .borrow_mut()
        .restore_parts()
        .0
        .starting(epoch, None);

    let mut backend = TestBackend::new([], SlashCommandOutcome::NotReady, Vec::new());

    backend.resume_outcome = ResumeOutcome::SwitchedInPlace;

    assert!(matches!(
        pane.session
            .borrow_mut()
            .runtime_mut()
            .install(epoch, Ok(Backend::Test(backend))),
        StartOutcome::Installed
    ));

    pane.session.borrow_mut().runtime_mut().ready();
}

/// Make the installed backend answer like a harness that picks its
/// conversation at launch, which is what sends a resume to the disk read.
fn resume_by_reading_history(pane: &mut AgentPane) {
    match pane.session.borrow_mut().runtime_mut().backend_mut() {
        Some(Backend::Test(backend)) => backend.resume_outcome = ResumeOutcome::NeedsReplayRead,
        _ => panic!("the test backend must be installed"),
    }
}

fn prepare_local_replay(pane: &mut AgentPane, cx: &App) -> RecoveryIdentity {
    resume_by_reading_history(pane);

    let cwd = pane.cwd(cx);

    let ResumeStart::ReadReplay(request) = pane
        .session
        .borrow_mut()
        .begin_resume(&summary(), cwd.as_deref())
    else {
        panic!("Claude restoration must read history");
    };

    let ReplayLoaded::Restart(identity) = pane.session.borrow_mut().replay_loaded(
        request,
        cwd.as_deref(),
        Ok(LoadedReplay {
            turns: replay("restored"),
            title: None,
        }),
    ) else {
        panic!("loaded history must prepare a restart");
    };

    pane.history_ui.mode = RecentSessionsMode::Loading;

    identity
}

#[gpui::test]
fn restored_conversation_resumes_once_on_the_first_ready(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    let (titles, _subscription) = collect_titles(&pane, &mut cx);

    let host = cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            install_backend(pane);
        });

        let host = pane.read(cx).agent_session().expect("pane has a session");

        host.update(cx, |session, _| session.resume_when_ready(summary()));

        host
    });

    // Nothing has replayed yet, so a snapshot taken now must still carry the
    // conversation the tab is about to continue.
    cx.update(|_, cx| {
        assert_eq!(
            host.read(cx).saved_conversation(cx).as_deref(),
            Some("selected")
        );
    });

    deliver_session_event(&pane, Event::Ready(ThreadSettings::default()), &cx);

    cx.update(|_, cx| {
        assert_eq!(
            pane.read(cx).session.borrow().runtime().status(),
            Status::Starting
        );
    });

    deliver_session_event(&pane, Event::Replay(replay("restored")), &cx);

    deliver_session_event(&pane, Event::Ready(ThreadSettings::default()), &cx);

    // The Ready that follows the replay must not start another resume.
    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            assert_eq!(user_rows(pane, cx), ["restored"]);
            assert_eq!(pane.session.borrow().runtime().status(), Status::Idle);
        })
    });

    // A tab opened for a conversation listed elsewhere is named after the
    // list's row, the same as a resume from its own list.
    assert_eq!(*titles.borrow(), ["Selected"]);
}

#[gpui::test]
fn in_place_resume_names_the_tab_after_the_picked_conversation(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            install_backend(pane);
        })
    });

    let (titles, _subscription) = collect_titles(&pane, &mut cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            pane.history_ui.data.sessions = vec![summary()];
            pane.history_ui.mode = RecentSessionsMode::Open;

            pane.resume_session(0, cx);
        })
    });

    // A harness that stores no name for the thread reports none on resume,
    // so the list's title is all that can name the tab.
    deliver_session_event(&pane, Event::Replay(replay("restored")), &cx);

    assert_eq!(*titles.borrow(), ["Selected"]);

    // The next prompt continues a named conversation, so it must not rename
    // the tab after itself.
    cx.update(|_, cx| {
        let request = pane
            .read(cx)
            .session
            .borrow()
            .title_request("next prompt", |_| None);

        assert!(request.is_none());
    });
}

#[gpui::test]
fn history_read_resume_names_the_tab_after_the_picked_conversation(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            install_backend(pane);
        })
    });

    let (titles, _subscription) = collect_titles(&pane, &mut cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            let identity = prepare_local_replay(pane, cx);
            let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

            pane.session
                .borrow_mut()
                .restore_parts()
                .0
                .starting(epoch, Some(&identity));
        })
    });

    deliver_session_event(&pane, Event::Ready(ThreadSettings::default()), &cx);

    assert_eq!(*titles.borrow(), ["Selected"]);
}

#[gpui::test]
fn failed_resume_keeps_the_transcript_and_current_controls(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            install_backend(pane);
        })
    });

    deliver_session_event(&pane, Event::Replay(replay("current")), &cx);

    let settings = cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            pane.session
                .borrow_mut()
                .controls
                .set_model("current-model".into());

            let settings = pane.session.borrow().controls.settings.clone();

            pane.history_ui.data.sessions = vec![summary()];
            pane.history_ui.mode = RecentSessionsMode::Open;

            pane.resume_session(0, cx);

            assert_eq!(pane.history_ui.mode, RecentSessionsMode::Loading);
            assert_eq!(user_rows(pane, cx), ["current"]);

            settings
        })
    });

    deliver_session_event(
        &pane,
        Event::Error {
            message: "resume rejected".into(),
            fatal: false,
        },
        &cx,
    );

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            assert_eq!(pane.history_ui.mode, RecentSessionsMode::Open);
            assert_eq!(pane.session.borrow().runtime().status(), Status::Idle);
            assert_eq!(user_rows(pane, cx), ["current"]);
            assert_eq!(pane.session.borrow().controls.settings, settings);
        })
    });
}

#[gpui::test]
fn failed_replacement_keeps_old_rows_and_never_publishes_pending_history(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            install_backend(pane);
        })
    });

    deliver_session_event(&pane, Event::Replay(replay("current")), &cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            let identity = prepare_local_replay(pane, cx);
            let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

            pane.session
                .borrow_mut()
                .restore_parts()
                .0
                .starting(epoch, Some(&identity));

            assert_eq!(
                pane.install_started_session(Err("cannot spawn".into()), epoch, "Claude", cx),
                Some(false)
            );

            {
                let mut guard = pane.session.borrow_mut();

                let (restore, runtime) = guard.restore_parts();

                restore.failed(runtime)
            };

            assert_eq!(pane.session.borrow().runtime().status(), Status::Exited);
            assert_eq!(user_rows(pane, cx), ["current"]);
            assert!(!matches!(
                pane.session.borrow_mut().restore_parts().0.ready(epoch),
                ReadyAction::Replay(_)
            ));
        })
    });
}

#[gpui::test]
fn local_history_waits_for_ready_and_repeated_ready_does_not_erase_new_rows(
    cx: &mut TestAppContext,
) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            install_backend(pane);
        })
    });

    deliver_session_event(&pane, Event::Replay(replay("current")), &cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            let identity = prepare_local_replay(pane, cx);
            let epoch = pane.session.borrow_mut().runtime_mut().begin_start();

            pane.session
                .borrow_mut()
                .restore_parts()
                .0
                .starting(epoch, Some(&identity));

            assert_eq!(user_rows(pane, cx), ["current"]);
        })
    });

    deliver_session_event(&pane, Event::Ready(ThreadSettings::default()), &cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            assert_eq!(user_rows(pane, cx), ["restored"]);
            assert_eq!(pane.history_ui.mode, RecentSessionsMode::Hidden);
        })
    });

    deliver_session_event(&pane, Event::Replay(replay("new prompt")), &cx);

    deliver_session_event(&pane, Event::Ready(ThreadSettings::default()), &cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            assert_eq!(user_rows(pane, cx), ["restored", "new prompt"]);
        })
    });
}

#[gpui::test]
fn resumed_codex_controls_keep_provider_values_instead_of_local_defaults(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    let settings = cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            install_backend(pane);

            pane.history_ui.data.sessions = vec![summary()];
            pane.history_ui.mode = RecentSessionsMode::Open;

            pane.session
                .borrow_mut()
                .controls
                .set_model("old-model".into());

            pane.resume_session(0, cx);

            assert_eq!(pane.session.borrow().controls.seed, SettingsSeed::Reviewer);

            ThreadSettings {
                model: Some("resumed-model".into()),
                ..ThreadSettings::default()
            }
        })
    });

    deliver_session_event(&pane, Event::Ready(settings), &cx);

    deliver_session_event(&pane, Event::Replay(replay("restored")), &cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            assert_eq!(
                pane.session.borrow().controls.settings.model.as_deref(),
                Some("resumed-model")
            );
            assert_eq!(user_rows(pane, cx), ["restored"]);

            pane.seed_restored_settings(SettingsSeed::None);

            assert_eq!(pane.session.borrow().controls.seed, SettingsSeed::None);
        })
    });
}

#[gpui::test]
fn old_backend_events_during_disk_read_leave_visible_rows_and_settings_untouched(
    cx: &mut TestAppContext,
) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            install_backend(pane);
        })
    });

    deliver_session_event(&pane, Event::Replay(replay("current")), &cx);

    let (settings, cwd, request) = cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            pane.session
                .borrow_mut()
                .controls
                .set_model("current-model".into());

            let settings = pane.session.borrow().controls.settings.clone();

            resume_by_reading_history(pane);

            let cwd = pane.cwd(cx);

            let ResumeStart::ReadReplay(request) = pane
                .session
                .borrow_mut()
                .begin_resume(&summary(), cwd.as_deref())
            else {
                panic!("Claude restoration must read history");
            };

            pane.history_ui.mode = RecentSessionsMode::Loading;

            (settings, cwd, request)
        })
    });

    deliver_session_event(
        &pane,
        Event::Ready(ThreadSettings {
            model: Some("old-handshake".into()),
            ..ThreadSettings::default()
        }),
        &cx,
    );

    deliver_session_event(&pane, Event::Replay(replay("old-handshake")), &cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            assert_eq!(pane.session.borrow().runtime().status(), Status::Starting);
            assert_eq!(pane.history_ui.mode, RecentSessionsMode::Loading);
            assert_eq!(user_rows(pane, cx), ["current"]);
            assert_eq!(pane.session.borrow().controls.settings, settings);
            assert!(matches!(
                pane.session.borrow_mut().replay_loaded(
                    request,
                    cwd.as_deref(),
                    Ok(LoadedReplay {
                        turns: replay("restored"),
                        title: None,
                    }),
                ),
                ReplayLoaded::Restart(_)
            ));
        })
    });
}
