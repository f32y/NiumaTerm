use std::time::SystemTime;

use gpui::{App, AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};
use gpui_component::Root;
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::{
    Event, Item, ReplayItem, ReplayTurn, SessionSummary, SlashCommandOutcome, ThreadSettings,
};
use nmt_agent::session::lifecycle::{StartOutcome, Status};
use nmt_agent::session::restore::{ReadyAction, ReplayLoaded, ResumeStart, SettingsSeed};
use nmt_agent::session::{AgentKind, RecoveryIdentity};
use nmt_config::profile::{AgentProfile, AgentProfileKind};

use crate::agent_tab::session::{Backend, TestBackend};
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::tests::deliver_session_event;
use crate::agent_tab::{AgentPane, AgentThreadDefaults, RecentSessionsMode};

fn open_pane(cx: &mut TestAppContext) -> (Entity<AgentPane>, WindowHandle<Root>) {
    let profile = AgentProfile {
        name: "Restore Test".into(),
        kind: AgentProfileKind::Codex,
        // The test installs a backend before the initial async start is polled.
        executable: "missing-agent.exe".into(),
        ..AgentProfile::default()
    };

    let mut pane = None;

    let window = cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(AgentSettings::default());
        cx.set_global(AgentThreadDefaults::default());

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

fn install_backend(pane: &mut AgentPane) {
    let epoch = pane.session.borrow_mut().runtime.begin_start();

    pane.session.borrow_mut().restore.starting(epoch, None);

    let mut backend = TestBackend::new([], SlashCommandOutcome::NotReady, Vec::new());

    backend.resume_accepted = true;

    assert!(matches!(
        pane.session
            .borrow_mut()
            .runtime
            .install(epoch, Ok(Backend::Test(backend))),
        StartOutcome::Installed
    ));

    pane.session.borrow_mut().runtime.ready();
}

fn prepare_local_replay(pane: &mut AgentPane, cx: &App) -> RecoveryIdentity {
    let cwd = pane.cwd(cx);

    let ResumeStart::ReadReplay(request) = ({
        let mut guard = pane.session.borrow_mut();
        let state = &mut *guard;

        state.restore.begin(
            &mut state.runtime,
            AgentKind::Claude,
            &summary(),
            cwd.as_deref(),
        )
    }) else {
        panic!("Claude restoration must read history");
    };

    let ReplayLoaded::Restart(identity) = ({
        let mut guard = pane.session.borrow_mut();
        let state = &mut *guard;

        state.restore.loaded(
            &mut state.runtime,
            request,
            cwd.as_deref(),
            Ok(replay("restored")),
        )
    }) else {
        panic!("loaded history must prepare a restart");
    };

    pane.history_ui.mode = RecentSessionsMode::Loading;

    identity
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
            assert_eq!(pane.session.borrow().runtime.status(), Status::Idle);
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
            let epoch = pane.session.borrow_mut().runtime.begin_start();

            pane.session
                .borrow_mut()
                .restore
                .starting(epoch, Some(&identity));

            assert_eq!(
                pane.install_started_session(Err("cannot spawn".into()), epoch, "Claude", cx),
                Some(false)
            );

            {
                let mut guard = pane.session.borrow_mut();
                let state = &mut *guard;

                state.restore.failed(&mut state.runtime)
            };

            assert_eq!(pane.session.borrow().runtime.status(), Status::Exited);
            assert_eq!(user_rows(pane, cx), ["current"]);
            assert!(!matches!(
                pane.session.borrow_mut().restore.ready(epoch),
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
            let epoch = pane.session.borrow_mut().runtime.begin_start();

            pane.session
                .borrow_mut()
                .restore
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
            let cwd = pane.cwd(cx);

            let ResumeStart::ReadReplay(request) = ({
                let mut guard = pane.session.borrow_mut();
                let state = &mut *guard;

                state.restore.begin(
                    &mut state.runtime,
                    AgentKind::Claude,
                    &summary(),
                    cwd.as_deref(),
                )
            }) else {
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
            assert_eq!(pane.session.borrow().runtime.status(), Status::Starting);
            assert_eq!(pane.history_ui.mode, RecentSessionsMode::Loading);
            assert_eq!(user_rows(pane, cx), ["current"]);
            assert_eq!(pane.session.borrow().controls.settings, settings);
            assert!(matches!(
                {
                    let mut guard = pane.session.borrow_mut();
                    let state = &mut *guard;
                    state.restore.loaded(
                        &mut state.runtime,
                        request,
                        cwd.as_deref(),
                        Ok(replay("restored")),
                    )
                },
                ReplayLoaded::Restart(_)
            ));
        })
    });
}
