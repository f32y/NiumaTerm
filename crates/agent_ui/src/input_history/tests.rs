use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use std::{env, fs, process};

use gpui::{Entity, Image, ImageFormat, TestAppContext, VisualTestContext, WindowHandle};
use image_rs::{DynamicImage, ImageFormat as EncodedImageFormat, RgbaImage};
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::{SendOutcome, SessionSummary, SlashCommandOutcome};
use nmt_agent::codex::app_server;
use nmt_agent::input_history::AgentInputHistory as InputHistoryService;
use nmt_agent::session::lifecycle::StartOutcome;
use nmt_config::profile::{AgentProfile, AgentProfileKind};

use crate::composer::PaletteControl;
use crate::input_history::{
    AgentInputHistory, InputHistoryAction, InputHistoryDirection, InputHistoryNavigation,
    InputHistoryScope, replace_input_with_history,
};
use crate::session::{Backend, TestBackend};
use crate::settings::AgentSettings;
use crate::{AgentKind, AgentPane, AgentThreadDefaults, RecentSessionsMode};

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = env::temp_dir().join(format!(
            "niumaterm-input-history-test-{}-{}",
            process::id(),
            NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));

        fs::create_dir_all(&path).expect("create test directory");

        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scope(_target: &str, kind: AgentKind, cwd: &Path) -> InputHistoryScope {
    InputHistoryScope::local(
        kind,
        &AgentWorkspace::single(cwd.to_str().map(str::to_string)),
    )
}

/// A scope for a workspace whose primary directory is `cwd` and which also
/// owns `additional`.
fn multi_root_scope(kind: AgentKind, cwd: &Path, additional: &[&Path]) -> InputHistoryScope {
    InputHistoryScope::local(
        kind,
        &AgentWorkspace::new(
            cwd.to_str().map(str::to_string),
            additional
                .iter()
                .filter_map(|path| path.to_str().map(str::to_string))
                .collect(),
        ),
    )
}

fn open_test_pane(
    cx: &mut TestAppContext,
    directory: &TestDirectory,
) -> (Entity<AgentPane>, WindowHandle<gpui_component::Root>) {
    use gpui::AppContext as _;

    let profile = AgentProfile {
        name: "Input History Test".into(),
        kind: AgentProfileKind::Codex,
        executable: directory
            .path()
            .join("missing-agent.exe")
            .to_string_lossy()
            .into_owned(),
        ..AgentProfile::default()
    };

    let cwd = directory.path().to_string_lossy().into_owned();
    let history_path = directory.path().join("agent-input-history.json");
    let mut pane = None;

    let window = cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(AgentSettings::default());
        cx.set_global(AgentThreadDefaults::default());
        cx.set_global(AgentInputHistory(InputHistoryService::open(history_path)));

        cx.open_window(Default::default(), |window, cx| {
            let agent =
                cx.new(|cx| AgentPane::new(profile, AgentWorkspace::single(Some(cwd)), window, cx));

            pane = Some(agent.clone());

            cx.new(|cx| gpui_component::Root::new(agent, window, cx))
        })
        .expect("open Agent test window")
    });

    (pane.expect("create Agent pane"), window)
}

#[test]
fn navigation_moves_without_wrapping_and_clears_after_newest() {
    let entries: Arc<[String]> = Arc::from(vec!["oldest".into(), "newest".into()]);
    let mut navigation = InputHistoryNavigation::default();

    assert_eq!(
        navigation.navigate(InputHistoryDirection::Older, "", 0..0, 0, entries.clone()),
        InputHistoryAction::Replace("newest".into())
    );
    assert_eq!(
        navigation.navigate(
            InputHistoryDirection::Older,
            "newest",
            6..6,
            6,
            entries.clone()
        ),
        InputHistoryAction::Replace("oldest".into())
    );
    assert_eq!(
        navigation.navigate(
            InputHistoryDirection::Older,
            "oldest",
            0..0,
            0,
            entries.clone()
        ),
        InputHistoryAction::Keep
    );
    assert_eq!(
        navigation.navigate(
            InputHistoryDirection::Newer,
            "oldest",
            6..6,
            6,
            entries.clone()
        ),
        InputHistoryAction::Replace("newest".into())
    );
    assert_eq!(
        navigation.navigate(InputHistoryDirection::Newer, "newest", 6..6, 6, entries),
        InputHistoryAction::Clear
    );
}

#[test]
fn drafts_selections_and_interior_cursors_keep_editor_navigation() {
    let entries: Arc<[String]> = Arc::from(vec!["first".into(), "second".into()]);
    let mut navigation = InputHistoryNavigation::default();

    assert_eq!(
        navigation.navigate(
            InputHistoryDirection::Older,
            "draft",
            5..5,
            5,
            entries.clone()
        ),
        InputHistoryAction::Declined
    );
    assert_eq!(
        navigation.navigate(
            InputHistoryDirection::Older,
            "draft",
            0..5,
            5,
            entries.clone()
        ),
        InputHistoryAction::Declined
    );
    assert_eq!(
        navigation.navigate(InputHistoryDirection::Older, "", 0..0, 0, entries.clone()),
        InputHistoryAction::Replace("second".into())
    );
    assert_eq!(
        navigation.navigate(
            InputHistoryDirection::Older,
            "second",
            2..2,
            2,
            entries.clone()
        ),
        InputHistoryAction::Declined
    );
    assert_eq!(
        navigation.navigate(InputHistoryDirection::Older, "edited", 6..6, 6, entries),
        InputHistoryAction::Declined
    );
}

#[test]
fn multiline_and_slash_entries_are_plain_text_during_navigation() {
    let entries: Arc<[String]> = Arc::from(vec!["/status".into(), "first\nsecond".into()]);
    let mut navigation = InputHistoryNavigation::default();

    assert_eq!(
        navigation.navigate(InputHistoryDirection::Older, "", 0..0, 0, entries.clone()),
        InputHistoryAction::Replace("first\nsecond".into())
    );
    assert_eq!(
        navigation.navigate(
            InputHistoryDirection::Older,
            "first\nsecond",
            12..12,
            12,
            entries
        ),
        InputHistoryAction::Replace("/status".into())
    );
}

#[test]
fn matching_tabs_start_from_the_latest_shared_snapshot() {
    let directory = TestDirectory::new();
    let scope = scope("local", AgentKind::Codex, directory.path());
    let mut history = InputHistoryService::open(directory.path().join("history.json"));

    history.record(&scope, "first".into());

    let mut first_tab = InputHistoryNavigation::default();

    assert_eq!(
        first_tab.navigate(
            InputHistoryDirection::Older,
            "",
            0..0,
            0,
            history.entries(&scope)
        ),
        InputHistoryAction::Replace("first".into())
    );

    history.record(&scope, "second".into());

    let mut second_tab = InputHistoryNavigation::default();

    assert_eq!(
        second_tab.navigate(
            InputHistoryDirection::Older,
            "",
            0..0,
            0,
            history.entries(&scope)
        ),
        InputHistoryAction::Replace("second".into())
    );
}

#[gpui::test]
fn pane_navigation_keeps_palette_and_recent_sessions_ahead_of_history(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let (pane, window) = open_test_pane(cx, &directory);
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            cx.global_mut::<AgentInputHistory>()
                .record(&pane.input_history_scope, "history entry".into());

            pane.input
                .update(cx, |input, cx| input.set_value("/", window, cx));
            pane.palette.dismissed = false;
            pane.handle_palette_control(PaletteControl::Previous, window, cx);

            assert_eq!(pane.input.read(cx).text().to_string(), "/");
            assert!(pane.input_history_navigation.index.is_none());

            pane.input
                .update(cx, |input, cx| input.set_value("", window, cx));
            pane.history_ui.mode = RecentSessionsMode::Open;
            pane.history_ui.data.sessions = vec![SessionSummary {
                id: "session-1".into(),
                title: "Earlier session".into(),
                branch: None,
                cwd: None,
                last_active: SystemTime::now(),
                snippet: None,
            }];
            pane.handle_palette_control(PaletteControl::Previous, window, cx);

            assert_eq!(pane.input.read(cx).text().len(), 0);
            assert!(pane.input_history_navigation.index.is_none());

            pane.history_ui.mode = RecentSessionsMode::Hidden;
            pane.input
                .update(cx, |input, cx| input.set_value("draft", window, cx));
            pane.handle_palette_control(PaletteControl::Previous, window, cx);

            assert_eq!(pane.input.read(cx).text().to_string(), "draft");
            assert!(pane.input_history_navigation.index.is_none());

            pane.input
                .update(cx, |input, cx| input.set_value("", window, cx));
            pane.handle_palette_control(PaletteControl::Previous, window, cx);

            assert_eq!(pane.input.read(cx).text().to_string(), "history entry");
        });
    });
}

#[gpui::test]
fn accepted_new_turn_and_steering_record_only_typed_input(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let (pane, window) = open_test_pane(cx, &directory);
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let epoch = pane.runtime.begin_start();

            assert!(matches!(
                pane.runtime.install(
                    epoch,
                    Ok(Backend::Test(TestBackend::new(
                        [
                            SendOutcome::StartedTurn,
                            SendOutcome::Steered,
                            SendOutcome::Steered,
                        ],
                        SlashCommandOutcome::NotReady,
                        Vec::new(),
                    )))
                ),
                StartOutcome::Installed
            ));

            pane.runtime.ready();

            pane.input.update(cx, |input, cx| {
                input.set_value("  start the turn  ", window, cx)
            });
            pane.send_user_message(window, cx);
            pane.input.update(cx, |input, cx| {
                input.set_value("steer the turn", window, cx)
            });
            pane.send_user_message(window, cx);

            assert!(pane.send_text("/effort high".into(), cx));

            assert_eq!(
                &*cx.global::<AgentInputHistory>()
                    .entries(&pane.input_history_scope),
                ["start the turn", "steer the turn"]
            );
            assert_eq!(pane.input.read(cx).text().len(), 0);
        });
    });
}

#[gpui::test]
fn slash_history_requires_a_successful_action(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let (pane, window) = open_test_pane(cx, &directory);
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let epoch = pane.runtime.begin_start();

            assert!(matches!(
                pane.runtime.install(
                    epoch,
                    Ok(Backend::Test(TestBackend::new(
                        [],
                        SlashCommandOutcome::Accepted,
                        app_server::Session::adapter_commands(),
                    )))
                ),
                StartOutcome::Installed
            ));

            pane.runtime.ready();
            pane.input
                .update(cx, |input, cx| input.set_value("/compact", window, cx));
            pane.submit_current_slash(window, cx);

            let epoch = pane.runtime.begin_start();

            assert!(matches!(
                pane.runtime.install(
                    epoch,
                    Ok(Backend::Test(TestBackend::new(
                        [],
                        SlashCommandOutcome::Rejected {
                            message: "rejected".into(),
                        },
                        app_server::Session::adapter_commands(),
                    )))
                ),
                StartOutcome::Installed
            ));

            pane.runtime.ready();
            pane.palette.commands.awaiting_turn = false;
            pane.input
                .update(cx, |input, cx| input.set_value("/review", window, cx));
            pane.submit_current_slash(window, cx);

            assert_eq!(pane.input.read(cx).text().to_string(), "/review");

            pane.input
                .update(cx, |input, cx| input.set_value("/missing", window, cx));
            pane.submit_current_slash(window, cx);

            assert_eq!(pane.input.read(cx).text().to_string(), "/missing");

            pane.input
                .update(cx, |input, cx| input.set_value("/status", window, cx));
            pane.submit_current_slash(window, cx);

            assert_eq!(
                &*cx.global::<AgentInputHistory>()
                    .entries(&pane.input_history_scope),
                ["/compact", "/status"]
            );
        });
    });
}

#[gpui::test]
fn rejected_submission_preserves_draft_images_and_unnamed_state(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let (pane, window) = open_test_pane(cx, &directory);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let mut bytes = Cursor::new(Vec::new());

    DynamicImage::ImageRgba8(RgbaImage::new(1, 1))
        .write_to(&mut bytes, EncodedImageFormat::Png)
        .unwrap();

    let image = Image::from_bytes(ImageFormat::Png, bytes.into_inner());

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let epoch = pane.runtime.begin_start();

            assert!(matches!(
                pane.runtime.install(
                    epoch,
                    Ok(Backend::Test(TestBackend::new(
                        [SendOutcome::Rejected {
                            message: "input queue unavailable".into(),
                        }],
                        SlashCommandOutcome::NotReady,
                        Vec::new(),
                    )))
                ),
                StartOutcome::Installed
            ));

            pane.runtime.ready();
            pane.naming.named = false;
            pane.input.update(cx, |input, cx| {
                input.set_value("keep this draft", window, cx)
            });
            pane.attachments
                .attach_image(&image, &pane.input, window, cx)
                .ok()
                .expect("attach test image");

            let draft = pane.input.read(cx).text().to_string();

            pane.send_user_message(window, cx);

            assert_eq!(pane.input.read(cx).text().to_string(), draft);
            assert_eq!(pane.attachments.images().iter().count(), 1);
            assert!(!pane.naming.named);
            assert!(
                cx.global::<AgentInputHistory>()
                    .entries(&pane.input_history_scope)
                    .is_empty()
            );
        });
    });
}

#[gpui::test]
fn unavailable_session_keeps_input_without_recording(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let (pane, window) = open_test_pane(cx, &directory);
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.runtime.retire();
            pane.runtime.begin_conversation_change();
            pane.input
                .update(cx, |input, cx| input.set_value("not accepted", window, cx));
            pane.send_user_message(window, cx);

            assert_eq!(pane.input.read(cx).text().to_string(), "not accepted");
            assert!(
                cx.global::<AgentInputHistory>()
                    .entries(&pane.input_history_scope)
                    .is_empty()
            );
        });
    });
}

#[gpui::test]
fn restored_multiline_text_places_the_utf8_cursor_at_the_end(cx: &mut gpui::TestAppContext) {
    use gpui::{AppContext as _, VisualTestContext};
    use gpui_component::input::TextareaState;

    let mut input = None;

    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            gpui_component::init(cx);

            let state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 8));

            input = Some(state.clone());

            cx.new(|cx| gpui_component::Root::new(state, window, cx))
        })
        .expect("open test window")
    });

    let input = input.expect("create input state");
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let text = "词元\n/status".to_string();
    let expected_end = text.len();

    cx.update(|window, cx| {
        let owner = cx.new(|_| ());

        owner.update(cx, |_, owner_cx| {
            replace_input_with_history(&input, text, window, owner_cx);
        });
    });

    cx.update(|_, cx| {
        let input = input.read(cx);

        assert_eq!(input.text().to_string(), "词元\n/status");
        assert_eq!(input.cursor(), expected_end);
        assert_eq!(input.selected_range(), expected_end..expected_end);
    });
}

#[gpui::test]
async fn editing_a_workspace_reaches_the_next_conversation_only(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let (pane, _window) = open_test_pane(cx, &directory);

    let started = AgentWorkspace::single(Some(directory.path().to_string_lossy().into_owned()));

    let edited = AgentWorkspace::new(
        Some(directory.path().to_string_lossy().into_owned()),
        vec![
            directory
                .path()
                .join("attached")
                .to_string_lossy()
                .into_owned(),
        ],
    );

    pane.update(cx, |pane, cx| {
        // The pane's first start already cloned its configured list.
        assert_eq!(pane.active_workspace(), &started);

        pane.set_workspace(edited.clone(), cx);

        // The conversation in flight keeps what it was granted; only the
        // configured list moved.
        assert_eq!(pane.configured_workspace(), &edited);
        assert_eq!(pane.active_workspace(), &started);

        // The scope follows the configured list, so the edited workspace has
        // its own prompt history from here on.
        assert_eq!(
            pane.input_history_scope,
            multi_root_scope(
                AgentKind::Codex,
                directory.path(),
                &[&directory.path().join("attached")],
            )
        );

        cx.global_mut::<AgentInputHistory>()
            .record(&pane.input_history_scope, "after the edit".into());
    });

    // The prompts recorded before the edit stay with the root set they were
    // typed in.
    let entries = cx.update(|cx| {
        cx.global::<AgentInputHistory>().entries(&scope(
            "local",
            AgentKind::Codex,
            directory.path(),
        ))
    });

    assert!(entries.is_empty());
}

#[gpui::test]
fn interruption_restores_only_unanswered_input_and_preserves_new_drafts(cx: &mut TestAppContext) {
    use nmt_agent::chat::{Event, Item};

    for visible in [false, true] {
        let directory = TestDirectory::new();
        let (pane, window) = open_test_pane(cx, &directory);
        let mut view_cx = VisualTestContext::from_window(window.into(), cx);

        view_cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                let epoch = pane.runtime.begin_start();
                let mut backend = TestBackend::new(
                    [SendOutcome::StartedTurn],
                    SlashCommandOutcome::NotReady,
                    Vec::new(),
                );
                backend.interrupt_accepted = true;
                assert!(matches!(
                    pane.runtime.install(epoch, Ok(Backend::Test(backend))),
                    StartOutcome::Installed
                ));
                pane.runtime.ready();
                pane.attachments.add_annotation("quoted answer".into());
                pane.input.update(cx, |input, cx| {
                    input.set_value("original draft", window, cx)
                });
                pane.send_user_message(window, cx);
                let turn = pane.delivery.turn();

                pane.start_item(
                    Item::AgentMessage {
                        id: "answer".into(),
                        text: None,
                        questions: None,
                    },
                    cx,
                );
                if visible {
                    pane.append_delta(
                        "answer",
                        "visible response",
                        |item| match item {
                            Item::AgentMessage { text, .. } => Some(text),
                            _ => None,
                        },
                        cx,
                    );
                }

                pane.input
                    .update(cx, |input, cx| input.set_value("new draft", window, cx));
                pane.interrupt_from_ui(window, cx);
                let input = pane.input.read(cx).text().to_string();

                if visible {
                    assert_eq!(input, "new draft");
                    assert!(pane.attachments.annotations().is_empty());
                    assert!(pane.delivery.is_active());
                    pane.apply_event(Event::TurnStarted, cx);
                    assert_eq!(pane.delivery.turn(), turn);
                } else {
                    assert!(input.contains("original draft"));
                    assert!(input.ends_with("new draft"));
                    assert_eq!(pane.attachments.annotations(), ["quoted answer"]);
                    assert!(!pane.transcript.read(cx).is_working());
                    assert!(!pane.delivery.is_active());

                    pane.apply_event(Event::TurnStarted, cx);
                    assert_eq!(pane.delivery.turn(), turn + 1);
                    assert!(pane.transcript.read(cx).is_working());
                }

                assert_eq!(
                    cx.global::<AgentInputHistory>()
                        .entries(&pane.input_history_scope)
                        .len(),
                    1,
                    "recovering a draft must not record a second submission"
                );
            });
        });
    }
}
