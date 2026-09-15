use std::cell::Cell;
use std::rc::Rc;

use app::agent_tab::AgentKind;
use app::agent_tab::settings::AgentSettings;
use app::agent_tab::team::{TeamPane, TeamRuntime};
use gpui::{AppContext as _, TestAppContext};
use gpui_component::input::InputState;
use nmt_agent::AgentWorkspace;
use nmt_agent::team::session::TeamSession;
use nmt_config::local_state::TabState;
use tempfile::tempdir;

use crate::ui::shell::close_confirm::should_confirm_close;
use crate::ui::shell::{InlineRename, InlineRenameStyle, TabSurface};

struct InlineRenameProbe {
    input: gpui::Entity<InputState>,
    cancelled: Rc<Cell<bool>>,
}

impl gpui::Render for InlineRenameProbe {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let cancelled = self.cancelled.clone();

        InlineRename::new(
            "inline-rename-probe",
            "Rename probe",
            self.input.clone(),
            InlineRenameStyle::HorizontalTab,
            move |_, _| cancelled.set(true),
        )
    }
}

#[gpui::test]
fn restored_agent_tab_keeps_kind_before_activation(cx: &mut TestAppContext) {
    let surface = TabSurface::Pending(Box::new(TabState {
        agent: Some("codex".to_string()),
        ..TabState::default()
    }));

    assert!(cx.update(|cx| surface.agent_kind(cx)) == Some(AgentKind::Codex));
}

#[gpui::test]
fn disabling_agent_team_releases_the_runtime_and_keeps_the_saved_room(cx: &mut TestAppContext) {
    let directory = tempdir().unwrap();

    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());
    });

    let cx = cx.add_empty_window();

    let (runtime, room_id) = cx.update(|window, cx| {
        let runtime = TeamRuntime::create(directory.path(), AgentWorkspace::default(), cx).unwrap();
        let room_id = runtime.read(cx).room().id();
        let weak = runtime.downgrade();
        let pane = cx.new(|cx| TeamPane::new(runtime, window, cx));

        let mut surface = TabSurface::Team(pane);

        assert!(surface.disable_team(cx));

        let TabSurface::TeamDisabled(saved) = surface else {
            panic!("disabled team retained its live pane");
        };

        assert_eq!(saved.team_room, Some(room_id.to_string()));

        (weak, room_id)
    });

    cx.run_until_parked();

    assert!(runtime.upgrade().is_none());

    let (restored, _) = TeamSession::open(directory.path(), room_id).unwrap();

    assert_eq!(restored.store().room().id(), room_id);
}

#[test]
fn window_close_honors_confirmation_setting() {
    use nmt_config::system::WarnBeforeTerminatingShell::{
        Always, Disabled, WhenChildProcessesRunning,
    };
    use std::io;

    for mode in [Disabled, WhenChildProcessesRunning, Always] {
        assert!(should_confirm_close(true, mode, &Ok(0)));
        assert!(should_confirm_close(
            true,
            mode,
            &Err(io::Error::other("query failed"))
        ));
    }

    assert!(!should_confirm_close(false, Disabled, &Ok(2)));
    assert!(!should_confirm_close(
        false,
        Disabled,
        &Err(io::Error::other("query failed"))
    ));
    assert!(!should_confirm_close(
        false,
        WhenChildProcessesRunning,
        &Ok(0)
    ));
    assert!(should_confirm_close(
        false,
        WhenChildProcessesRunning,
        &Ok(2)
    ));
    assert!(should_confirm_close(
        false,
        WhenChildProcessesRunning,
        &Err(io::Error::other("query failed"))
    ));
    assert!(should_confirm_close(false, Always, &Ok(0)));
}

#[gpui::test]
fn inline_rename_routes_escape_to_cancellation(cx: &mut TestAppContext) {
    use gpui::{AppContext as _, VisualTestContext};

    let cancelled = Rc::new(Cell::new(false));

    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            gpui_component::init(cx);

            let input = cx.new(|cx| InputState::new(window, cx).default_value("old name"));

            input.update(cx, |input, cx| input.focus(window, cx));

            let probe = cx.new(|_| InlineRenameProbe {
                input,
                cancelled: cancelled.clone(),
            });

            cx.new(|cx| gpui_component::Root::new(probe, window, cx))
        })
        .expect("open inline rename test window")
    });

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.run_until_parked();

    cx.refresh().expect("render inline rename probe");

    cx.simulate_keystrokes("escape");

    cx.run_until_parked();

    assert!(cancelled.get());
}

#[test]
fn right_side_views_share_one_area() {
    use crate::ui::right_panel::{RightPanelKind, RightPanelSelection};

    let mut selection = RightPanelSelection::new();

    assert!(selection.select(RightPanelKind::BackgroundTasks));
    assert!(selection.shows(RightPanelKind::BackgroundTasks));
    assert!(selection.select(RightPanelKind::Workflows));
    assert!(selection.shows(RightPanelKind::Workflows));
    assert!(!selection.shows(RightPanelKind::BackgroundTasks));
    assert!(!selection.select(RightPanelKind::Workflows));
    assert!(!selection.shows(RightPanelKind::Workflows));
}

/// Shared by the ready-tab and busy-tab jumps: each click walks to the next
/// marked tab and wraps, so a set of them is visited in order rather than the
/// same one being reopened.
#[test]
fn marked_tab_search_wraps_past_the_active_tab() {
    use crate::ui::shell::next_marked_position;

    let marks = [true, false, true, false];

    assert_eq!(next_marked_position(&marks, 0), Some(2));
    assert_eq!(next_marked_position(&marks, 2), Some(0));
    assert_eq!(next_marked_position(&marks, 3), Some(0));

    // The active tab is the last slot visited, so its own mark still counts
    // when nothing else carries one.
    assert_eq!(next_marked_position(&[true], 0), Some(0));

    assert_eq!(next_marked_position(&[false, false], 0), None);
    assert_eq!(next_marked_position(&[], 0), None);
}
