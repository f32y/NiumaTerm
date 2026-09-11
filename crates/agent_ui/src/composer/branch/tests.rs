use std::time::SystemTime;

use gpui::{
    App, AppContext as _, Context, Entity, TestAppContext, VisualTestContext, WindowHandle,
};
use gpui_component::Root;
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::{
    Event, ForkAnchor, ForkCheckpoint, Item, ReplayItem, ReplayTurn, SessionSummary,
    SlashCommandOutcome, ThreadSettings,
};
use nmt_agent::claude_code::sessions::{ClaudeCheckpoint, ClaudeFork, FileRestoreAvailability};
use nmt_agent::session::branch::{BranchUpdate, RewindAction};
use nmt_agent::session::lifecycle::{StartOutcome, Status};
use nmt_agent::session::test_support::TestBackend;
use nmt_agent::session::{AgentKind, Backend};
use nmt_config::profile::{AgentProfile, AgentProfileKind};

use crate::settings::AgentSettings;
use crate::{AgentPane, AgentThreadDefaults, RecentSessionsMode};

fn open_pane(cx: &mut TestAppContext) -> (Entity<AgentPane>, WindowHandle<Root>) {
    let profile = AgentProfile {
        name: "Branch Test".into(),
        kind: AgentProfileKind::Codex,
        // The initial async start is replaced before it is polled.
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
        .expect("open branch test window")
    });
    (pane.expect("create pane"), window)
}

fn install(pane: &mut AgentPane) {
    let epoch = pane.session.runtime.begin_start();
    pane.session.branch.starting(epoch, None);
    pane.session.restore.starting(epoch, None);
    let mut backend = TestBackend::new([], SlashCommandOutcome::Accepted, Vec::new())
        .with_recovery(AgentKind::Claude, "source");
    backend.fork_accepted = true;
    assert!(matches!(
        pane.session
            .runtime
            .install(epoch, Ok(Backend::Test(backend))),
        StartOutcome::Installed
    ));
    pane.session.runtime.ready();
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

fn rows(pane: &AgentPane, cx: &App) -> Vec<String> {
    pane.transcript
        .read(cx)
        .content
        .entries()
        .iter()
        .filter_map(|entry| match &entry.item {
            Item::UserMessage { text } => text.clone(),
            _ => None,
        })
        .collect()
}

fn fork(pane: &mut AgentPane, cx: &mut Context<AgentPane>) {
    assert!(pane.open_fork(cx));
    let checkpoint = ForkCheckpoint {
        prompt: "cut prompt".into(),
        timestamp: None,
        anchor: ForkAnchor::CodexThrough("turn".into()),
    };
    pane.apply_event(Event::ForkCheckpoints(Ok(vec![checkpoint.clone()])), cx);
    pane.start_conversation_branch(checkpoint, cx);
    assert!(pane.session.branch.is_working());
}

fn local_checkpoint() -> ClaudeCheckpoint {
    ClaudeCheckpoint {
        user_message_id: "prompt".into(),
        parent_message_id: None,
        prompt: "cut prompt".into(),
        timestamp: None,
        file_restore_availability: FileRestoreAvailability::Available,
    }
}

fn prepare_local(pane: &mut AgentPane, action: RewindAction, cx: &mut Context<AgentPane>) {
    let checkpoint = local_checkpoint();
    let request = pane
        .session
        .branch
        .begin_rewind(&pane.session.runtime, pane.cwd(), None)
        .expect("load");
    pane.session.branch.checkpoints_loaded(
        pane.session.runtime.epoch(),
        request,
        Ok(vec![checkpoint.clone()]),
    );
    assert!(
        pane.session
            .branch
            .select_checkpoint(pane.session.runtime.epoch(), checkpoint)
    );
    pane.branch.draft = Some(pane.input.read(cx).text().to_string());
    let update = pane
        .session
        .branch
        .rewind(&mut pane.session.runtime, action);
    let request = match update {
        BranchUpdate::CreateFork(request) => request,
        BranchUpdate::RestoringFiles(_) => {
            let BranchUpdate::CreateFork(request) = pane
                .session
                .branch
                .files_completed(pane.session.runtime.epoch(), Ok(()))
            else {
                panic!("fork after files")
            };
            request
        }
        _ => panic!("fork expected"),
    };
    let update = pane.session.branch.fork_created(
        pane.session.runtime.epoch(),
        request,
        Ok(ClaudeFork {
            session_id: Some("copy".into()),
            replay: replay("kept prefix"),
        }),
    );
    assert!(matches!(update, BranchUpdate::StartSession(_)));
    pane.apply_rewind_update(update, cx);
}

#[gpui::test]
fn protocol_branch_keeps_old_rows_until_replay_and_fills_the_prompt_once(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            install(pane);
            pane.apply_replay(replay("current"), cx);
            fork(pane, cx);
            pane.fill_branch_prompt(window, cx);
            assert_eq!(rows(pane, cx), ["current"]);
            assert!(pane.input.read(cx).text().len() == 0);

            pane.apply_event(Event::Ready(ThreadSettings::default()), cx);
            pane.apply_event(Event::Replay(replay("copy")), cx);
            assert_eq!(rows(pane, cx), ["copy"]);
            assert!(!pane.session.branch.holds_composer());
            pane.fill_branch_prompt(window, cx);
            assert_eq!(pane.input.read(cx).text().to_string(), "cut prompt");
            pane.input
                .update(cx, |input, cx| input.set_value("new draft", window, cx));
            pane.fill_branch_prompt(window, cx);
            assert_eq!(pane.input.read(cx).text().to_string(), "new draft");
        })
    });
}

#[gpui::test]
fn late_protocol_replay_does_not_overwrite_a_new_draft(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            install(pane);
            fork(pane, cx);
            pane.input
                .update(cx, |input, cx| input.set_value("new draft", window, cx));
            pane.apply_event(Event::Replay(replay("copy")), cx);
            pane.fill_branch_prompt(window, cx);
            assert_eq!(pane.input.read(cx).text().to_string(), "new draft");
            assert_eq!(rows(pane, cx), ["copy"]);
        })
    });
}

#[gpui::test]
fn protocol_failure_preserves_conversation_and_does_not_refill_the_prompt(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            install(pane);
            pane.apply_replay(replay("current"), cx);
            fork(pane, cx);
            pane.apply_event(
                Event::Error {
                    message: "fork rejected".into(),
                    fatal: false,
                },
                cx,
            );
            pane.fill_branch_prompt(window, cx);
            assert_eq!(rows(pane, cx), ["current"]);
            assert!(pane.input.read(cx).text().len() == 0);
            assert_eq!(pane.session.runtime.status(), Status::Idle);
            assert_eq!(pane.history_ui.mode, RecentSessionsMode::Open);
        })
    });
}

#[gpui::test]
fn local_branch_waits_for_ready_preserves_controls_and_keeps_later_drafts(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            install(pane);
            pane.apply_replay(replay("current"), cx);
            pane.session.controls.settings.model = Some("selected-model".into());
            prepare_local(pane, RewindAction::Conversation, cx);
            assert_eq!(rows(pane, cx), ["current"]);
            assert_eq!(pane.history_ui.mode, RecentSessionsMode::Loading);
            pane.input
                .update(cx, |input, cx| input.set_value("later draft", window, cx));

            pane.apply_event(Event::Ready(ThreadSettings::default()), cx);
            pane.fill_branch_prompt(window, cx);
            assert_eq!(rows(pane, cx), ["kept prefix"]);
            assert_eq!(
                pane.session.controls.settings.model.as_deref(),
                Some("selected-model")
            );
            assert_eq!(pane.input.read(cx).text().to_string(), "later draft");
            pane.apply_replay(replay("later turn"), cx);
            pane.apply_event(
                Event::Ready(ThreadSettings {
                    model: Some("selected-model".into()),
                    ..ThreadSettings::default()
                }),
                cx,
            );
            assert_eq!(rows(pane, cx), ["kept prefix", "later turn"]);
        })
    });
}

#[gpui::test]
fn local_start_failure_keeps_old_rows_and_reports_files_already_restored(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            install(pane);
            pane.apply_replay(replay("current"), cx);
            prepare_local(pane, RewindAction::FilesAndConversation, cx);
            assert!(matches!(
                pane.session
                    .runtime
                    .install(pane.session.runtime.epoch(), Err("cannot start".into())),
                StartOutcome::Failed(_)
            ));
            pane.apply_event(
                Event::Error {
                    message: "cannot start".into(),
                    fatal: true,
                },
                cx,
            );
            pane.fill_branch_prompt(window, cx);
            assert_eq!(rows(pane, cx), ["current"]);
            assert!(pane.input.read(cx).text().len() == 0);
            assert_eq!(pane.session.runtime.status(), Status::Exited);
            assert!(!pane.session.branch.holds_composer());
            let feedback = pane
                .palette
                .feedback
                .as_ref()
                .expect("partial completion must be reported");
            assert_eq!(
                feedback.message.as_ref(),
                nmt_i18n::i18n("agent-rewind-start-failed-after-files")
            );
        })
    });
}

#[gpui::test]
fn partial_success_picker_disables_repeating_files_but_allows_continuing_the_conversation(
    cx: &mut TestAppContext,
) {
    let (pane, window) = open_pane(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            install(pane);
            let checkpoint = local_checkpoint();
            let request = pane
                .session
                .branch
                .begin_rewind(&pane.session.runtime, pane.cwd(), None)
                .expect("read");
            pane.session.branch.checkpoints_loaded(
                pane.session.runtime.epoch(),
                request,
                Ok(vec![checkpoint.clone()]),
            );
            pane.session
                .branch
                .select_checkpoint(pane.session.runtime.epoch(), checkpoint);
            pane.session.branch.rewind(
                &mut pane.session.runtime,
                RewindAction::FilesAndConversation,
            );
            let BranchUpdate::CreateFork(request) = pane
                .session
                .branch
                .files_completed(pane.session.runtime.epoch(), Ok(()))
            else {
                panic!("fork expected")
            };
            let update = pane.session.branch.fork_created(
                pane.session.runtime.epoch(),
                request,
                Err("disk full".into()),
            );
            pane.apply_rewind_update(update, cx);
            let model = pane
                .palette_model(cx)
                .expect("retry actions remain visible");
            assert_eq!(
                model.rows[0].disabled_reason.as_deref(),
                Some(nmt_i18n::i18n("agent-rewind-files-restored"))
            );
            assert!(model.rows[1].disabled_reason.is_none());
            assert!(model.rows[2].disabled_reason.is_none());
        })
    });
}

#[gpui::test]
fn history_resume_cannot_take_over_an_open_branch_picker(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            install(pane);
            pane.history_ui.data.sessions = vec![SessionSummary {
                id: "other".into(),
                title: "Other".into(),
                branch: None,
                cwd: None,
                last_active: SystemTime::UNIX_EPOCH,
                snippet: None,
            }];
            pane.history_ui.mode = RecentSessionsMode::Open;
            assert!(pane.open_fork(cx));
            pane.resume_session(0, cx);
            assert_eq!(pane.history_ui.mode, RecentSessionsMode::Open);
            assert_eq!(pane.session.runtime.status(), Status::Idle);
            assert!(pane.session.branch.picker_is_open());
        })
    });
}
