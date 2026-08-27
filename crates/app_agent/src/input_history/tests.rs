use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use std::{env, fs, process};

use gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};
use nmt_agent_utils::AgentWorkspace;
use nmt_agent_utils::chat::{SendOutcome, SessionSummary, SlashCommandOutcome};
use nmt_config::profile::{AgentProfile, AgentProfileKind};

use crate::composer::PaletteControl;
use crate::input_history::store::{HistoryStore, load_from_path, save_to_path};
use crate::input_history::{
    AgentInputHistory, HistoryWriter, InputHistoryAction, InputHistoryDirection,
    InputHistoryNavigation, InputHistoryScope, replace_input_with_history,
};
use crate::session::{Backend, Status, TestBackend};
use crate::settings::AgentSettings;
use crate::{AgentKind, AgentPane, AgentThreadDefaults, RecentSessionsMode, app_server};

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

fn scope(target: &str, kind: AgentKind, cwd: &Path) -> InputHistoryScope {
    InputHistoryScope::new(
        target,
        kind,
        &AgentWorkspace::single(cwd.to_str().map(str::to_string)),
    )
}

/// A scope for a workspace whose primary directory is `cwd` and which also
/// owns `additional`.
fn multi_root_scope(kind: AgentKind, cwd: &Path, additional: &[&Path]) -> InputHistoryScope {
    InputHistoryScope::new(
        "local",
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
        cx.set_global(AgentInputHistory {
            store: HistoryStore::default(),
            path: history_path,
            writer: None,
        });
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
fn scope_uses_target_backend_and_normalized_directory() {
    let directory = TestDirectory::new();
    let first_cwd = directory.path().join("first");
    let second_cwd = directory.path().join("second");
    fs::create_dir_all(&first_cwd).expect("create first directory");
    fs::create_dir_all(&second_cwd).expect("create second directory");

    let local_codex = scope("local", AgentKind::Codex, &first_cwd);
    let equivalent = scope("local", AgentKind::Codex, &first_cwd.join("."));
    let local_claude = scope("local", AgentKind::Claude, &first_cwd);
    let remote_codex = scope("remote-a", AgentKind::Codex, &first_cwd);
    let other_cwd = scope("local", AgentKind::Codex, &second_cwd);

    assert_eq!(local_codex, equivalent);
    assert_ne!(local_codex, local_claude);
    assert_ne!(local_codex, remote_codex);
    assert_ne!(local_codex, other_cwd);

    let mut history = HistoryStore::default();
    history.record(&local_codex, "local codex".into());
    history.record(&local_claude, "local claude".into());
    history.record(&remote_codex, "remote codex".into());
    history.record(&other_cwd, "other directory".into());
    assert_eq!(history.entries(&local_codex), ["local codex"]);
    assert_eq!(history.entries(&local_claude), ["local claude"]);
    assert_eq!(history.entries(&remote_codex), ["remote codex"]);
    assert_eq!(history.entries(&other_cwd), ["other directory"]);
}

#[test]
fn recording_collapses_neighbors_and_keeps_the_newest_hundred() {
    let directory = TestDirectory::new();
    let codex_scope = scope("local", AgentKind::Codex, directory.path());
    let mut history = HistoryStore::default();

    assert!(history.record(&codex_scope, "first".into()));
    assert!(!history.record(&codex_scope, "first".into()));
    assert!(history.record(&codex_scope, "second".into()));
    assert!(history.record(&codex_scope, "first".into()));
    assert_eq!(history.entries(&codex_scope), ["first", "second", "first"]);

    let limited = scope("remote-a", AgentKind::Claude, directory.path());
    for index in 0..=100 {
        assert!(history.record(&limited, format!("entry-{index}")));
    }
    let entries = history.entries(&limited);
    assert_eq!(entries.len(), 100);
    assert_eq!(entries.first().map(String::as_str), Some("entry-1"));
    assert_eq!(entries.last().map(String::as_str), Some("entry-100"));
}

#[test]
fn json_round_trip_preserves_scoped_entries() {
    let directory = TestDirectory::new();
    let path = directory.path().join("agent-input-history.json");
    let codex = scope("local", AgentKind::Codex, directory.path());
    let claude = scope("local", AgentKind::Claude, directory.path());
    let mut history = HistoryStore::default();
    history.record(&codex, "line one\nline two".into());
    history.record(&claude, "/status".into());

    save_to_path(&path, &history.snapshot()).expect("save history");
    let restored = load_from_path(&path).expect("load history");

    assert_eq!(restored.entries(&codex), ["line one\nline two"]);
    assert_eq!(restored.entries(&claude), ["/status"]);
}

#[test]
fn missing_json_is_empty_and_invalid_json_is_reported() {
    let directory = TestDirectory::new();
    let missing = directory.path().join("missing.json");
    assert!(
        load_from_path(&missing)
            .expect("load missing history")
            .entries(&scope("local", AgentKind::Codex, directory.path()))
            .is_empty()
    );

    let invalid = directory.path().join("invalid.json");
    fs::write(&invalid, b"not json").expect("write invalid history");
    let error = load_from_path(&invalid).expect_err("invalid history must fail");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn failed_save_leaves_the_in_memory_entry_available() {
    let directory = TestDirectory::new();
    let blocker = directory.path().join("not-a-directory");
    fs::write(&blocker, b"blocked").expect("write blocker");
    let path = blocker.join("agent-input-history.json");
    let scope = scope("local", AgentKind::Codex, directory.path());
    let mut history = HistoryStore::default();
    history.record(&scope, "still available".into());

    assert!(save_to_path(&path, &history.snapshot()).is_err());
    assert_eq!(history.entries(&scope), ["still available"]);
}

#[test]
fn background_writer_flushes_the_latest_snapshot_in_order() {
    let directory = TestDirectory::new();
    let path = directory.path().join("agent-input-history.json");
    let scope = scope("local", AgentKind::Codex, directory.path());
    let writer = HistoryWriter::spawn(path.clone()).expect("start history writer");
    let mut history = AgentInputHistory {
        store: HistoryStore::default(),
        path: path.clone(),
        writer: Some(writer),
    };

    history.record(&scope, "first".into());
    history.record(&scope, "second".into());
    history.flush().expect("flush history");

    assert_eq!(
        load_from_path(&path).expect("load history").entries(&scope),
        ["first", "second"]
    );
}

#[test]
fn service_keeps_entries_when_background_writes_fail() {
    let directory = TestDirectory::new();
    let blocker = directory.path().join("not-a-directory");
    fs::write(&blocker, b"blocked").expect("write blocker");
    let path = blocker.join("agent-input-history.json");
    let scope = scope("local", AgentKind::Codex, directory.path());
    let writer = HistoryWriter::spawn(path.clone()).expect("start history writer");
    let mut history = AgentInputHistory {
        store: HistoryStore::default(),
        path,
        writer: Some(writer),
    };

    history.record(&scope, "still available".into());
    assert!(history.flush().is_err());
    assert_eq!(&*history.entries(&scope), ["still available"]);
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
    let mut history = HistoryStore::default();
    history.record(&scope, "first".into());

    let mut first_tab = InputHistoryNavigation::default();
    assert_eq!(
        first_tab.navigate(
            InputHistoryDirection::Older,
            "",
            0..0,
            0,
            Arc::from(history.entries(&scope))
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
            Arc::from(history.entries(&scope))
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
            pane.history_ui.sessions = vec![SessionSummary {
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
            pane.runtime.backend = Some(Backend::Test(TestBackend::new(
                [
                    SendOutcome::StartedTurn,
                    SendOutcome::Steered,
                    SendOutcome::Steered,
                ],
                SlashCommandOutcome::NotReady,
                Vec::new(),
            )));
            pane.runtime.status = Status::Idle;

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
            pane.runtime.backend = Some(Backend::Test(TestBackend::new(
                [],
                SlashCommandOutcome::Accepted,
                app_server::Session::adapter_commands(),
            )));
            pane.runtime.status = Status::Idle;
            pane.input
                .update(cx, |input, cx| input.set_value("/compact", window, cx));
            pane.submit_current_slash(window, cx);

            pane.runtime.backend = Some(Backend::Test(TestBackend::new(
                [],
                SlashCommandOutcome::Rejected {
                    message: "rejected".into(),
                },
                app_server::Session::adapter_commands(),
            )));
            pane.runtime.status = Status::Idle;
            pane.palette.awaiting_command_turn = false;
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
fn unavailable_session_keeps_input_without_recording(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let (pane, window) = open_test_pane(cx, &directory);
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.runtime.backend = None;
            pane.runtime.status = Status::Starting;
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
    use gpui_component::input::InputState;

    let mut input = None;
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            gpui_component::init(cx);
            let state = cx.new(|cx| InputState::new(window, cx).auto_grow(1, 8));
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

#[test]
fn workspaces_sharing_a_primary_directory_keep_separate_histories() {
    let directory = TestDirectory::new();
    let primary = directory.path().join("api");
    let web = directory.path().join("web");
    let docs = directory.path().join("docs");
    for path in [&primary, &web, &docs] {
        fs::create_dir_all(path).expect("create directory");
    }

    let alone = multi_root_scope(AgentKind::Codex, &primary, &[]);
    let with_web = multi_root_scope(AgentKind::Codex, &primary, &[&web]);
    let with_docs = multi_root_scope(AgentKind::Codex, &primary, &[&docs]);
    let both = multi_root_scope(AgentKind::Codex, &primary, &[&web, &docs]);
    let reordered = multi_root_scope(AgentKind::Codex, &primary, &[&docs, &web]);

    // Attaching a directory changes the working context, so the prompts
    // recorded in one root set do not surface in another.
    assert_ne!(alone, with_web);
    assert_ne!(with_web, with_docs);
    assert_ne!(both, reordered);

    // An equivalent spelling of the same ordered directories is the same
    // scope, so history survives a path written with other separators.
    let equivalent = multi_root_scope(AgentKind::Codex, &primary, &[&web.join(".")]);
    assert_eq!(with_web, equivalent);

    // A single-directory workspace still resolves to the scope that predates
    // multi-directory workspaces, which is what keeps its history reachable.
    assert_eq!(alone, scope("local", AgentKind::Codex, &primary));

    let mut history = HistoryStore::default();
    history.record(&alone, "alone".into());
    history.record(&with_web, "with web".into());
    history.record(&both, "both".into());
    assert_eq!(history.entries(&alone), ["alone"]);
    assert_eq!(history.entries(&with_web), ["with web"]);
    assert_eq!(history.entries(&both), ["both"]);
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
