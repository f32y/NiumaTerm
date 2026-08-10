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
