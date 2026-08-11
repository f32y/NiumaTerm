use gpui::TestAppContext;

use super::{
    AgentKind, TabState, TabSurface, WarnBeforeTerminatingShell, should_confirm_close,
    should_confirm_tab_close,
};

#[gpui::test]
fn restored_agent_tab_keeps_kind_before_activation(cx: &mut TestAppContext) {
    let surface = TabSurface::Pending(Box::new(TabState {
        agent: Some("codex".to_string()),
        ..TabState::default()
    }));

    assert!(cx.update(|cx| surface.agent_kind(cx)) == Some(AgentKind::Codex));
}

#[test]
fn window_close_honors_confirmation_setting() {
    use WarnBeforeTerminatingShell::Disabled;

    assert!(should_confirm_close(true, Disabled, 0));
    assert!(!should_confirm_close(false, Disabled, 0));
}

#[test]
fn agent_tab_close_honors_confirmation_setting() {
    use WarnBeforeTerminatingShell::{Always, Disabled};

    assert!(should_confirm_tab_close(true, true, Disabled, 0));
    assert!(!should_confirm_tab_close(true, false, Disabled, 0));
    assert!(!should_confirm_tab_close(false, true, Disabled, 0));
    assert!(should_confirm_tab_close(false, false, Always, 0));
}

/// The right-side area holds one content at a time, so Git and
/// `Background Tasks` cannot both consume a column.
#[test]
fn git_and_background_tasks_share_one_right_side_area() {
    use crate::ui::right_panel::{RightPanelKind, RightPanelSelection};

    let mut selection = RightPanelSelection::new();
    assert!(!selection.shows(RightPanelKind::Git));
    assert!(!selection.shows(RightPanelKind::BackgroundTasks));

    assert!(selection.select(RightPanelKind::Git));
    assert!(selection.shows(RightPanelKind::Git));

    // Selecting the other view replaces it rather than opening a second column.
    assert!(selection.select(RightPanelKind::BackgroundTasks));
    assert!(selection.shows(RightPanelKind::BackgroundTasks));
    assert!(!selection.shows(RightPanelKind::Git));

    // Selecting the visible view closes the area.
    assert!(!selection.select(RightPanelKind::BackgroundTasks));
    assert!(!selection.shows(RightPanelKind::BackgroundTasks));
    assert!(!selection.shows(RightPanelKind::Git));
}
