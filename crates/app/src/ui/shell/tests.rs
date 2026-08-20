use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Bounds, Pixels, TestAppContext};

use super::{
    AgentKind, TabState, TabSurface, WarnBeforeTerminatingShell, should_confirm_close,
    should_confirm_tab_close,
};
use crate::ui::shell::render::TAB_STRIP_MIN_WIDTH;
use crate::window::MIN_WINDOW_WIDTH;

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

/// The right-side area holds one content at a time, so Git,
/// `Background Tasks`, and `Workflows` cannot each consume a column.
#[test]
fn right_side_views_share_one_area() {
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

    // A third content joins the same rotation rather than adding a column.
    assert!(selection.select(RightPanelKind::Workflows));
    assert!(selection.shows(RightPanelKind::Workflows));
    assert!(!selection.shows(RightPanelKind::BackgroundTasks));
    assert!(!selection.shows(RightPanelKind::Git));

    // Selecting the visible view closes the area.
    assert!(!selection.select(RightPanelKind::Workflows));
    assert!(!selection.shows(RightPanelKind::Workflows));
    assert!(!selection.shows(RightPanelKind::BackgroundTasks));
    assert!(!selection.shows(RightPanelKind::Git));
}

/// Bounds captured from a laid-out title bar, keyed by group name.
type TitleBarProbe = Rc<RefCell<Vec<(&'static str, Bounds<Pixels>)>>>;

/// Replica of the shell's title bar: the same three groups around the same
/// `TitleBar` and `TabBar` components, with enough tabs to overflow any test
/// window. `left_width` stands in for the sidebar-aligned block, whose width
/// the user controls by dragging the sidebar edge.
struct TitleBarProbeView(TitleBarProbe, f32);

impl gpui::Render for TitleBarProbeView {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::prelude::*;
        use gpui::{div, px};
        use gpui_component::button::{Button, ButtonVariants as _};
        use gpui_component::tab::{Tab, TabBar, TabVariant};
        use gpui_component::{ElementExt as _, IconName, Sizable as _, TitleBar, h_flex};

        let rec = |name: &'static str, probe: TitleBarProbe| {
            move |bounds: Bounds<Pixels>, _: &mut gpui::Window, _: &mut gpui::App| {
                probe.borrow_mut().push((name, bounds));
            }
        };
        let probe = self.0.clone();

        let tab_bar = TabBar::new("probe-tabs")
            .with_variant(TabVariant::Modern)
            .large()
            .w_full()
            .min_w_0()
            .selected_index(0)
            .children(
                (0..12).map(|i| Tab::new().child(div().w(px(160.)).child(format!("tab {i}")))),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .on_prepaint(rec("root", probe.clone()))
            .child(
                TitleBar::new()
                    .child(
                        h_flex()
                            .w(px(self.1))
                            .min_w_0()
                            .overflow_hidden()
                            .on_prepaint(rec("left", probe.clone()))
                            .child(div().child(Button::new("a").ghost().icon(IconName::Settings)))
                            .child(div().child(Button::new("b").ghost().icon(IconName::Settings))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(TAB_STRIP_MIN_WIDTH))
                            .h_full()
                            .flex()
                            .items_center()
                            .on_prepaint(rec("tabs", probe.clone()))
                            .child(tab_bar),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .on_prepaint(rec("right", probe.clone()))
                            .child(div().child(Button::new("c").ghost().icon(IconName::Settings)))
                            .child(div().child(Button::new("d").ghost().icon(IconName::Settings))),
                    ),
            )
            .child(div().flex_1())
    }
}

/// More tabs than fit must squeeze the tab strip, never carry the right-hand
/// controls off the window: the strip scrolls horizontally, the controls do
/// not move out of reach. The widest sidebar-aligned block is used because it
/// is the layout's worst case.
#[gpui::test]
fn title_bar_controls_stay_inside_a_narrow_window(cx: &mut TestAppContext) {
    use gpui::{VisualTestContext, px, size};

    use crate::ui::workspace_sidebar::MAX_WIDTH;

    cx.update(|cx| gpui_component::init(cx));

    let probe: TitleBarProbe = Default::default();
    let handle = cx.add_window({
        let probe = probe.clone();
        move |_, _| TitleBarProbeView(probe, MAX_WIDTH)
    });
    let mut cx = VisualTestContext::from_window(handle.into(), cx);

    for width in [1200.0f32, 900.0, 700.0, MIN_WINDOW_WIDTH] {
        probe.borrow_mut().clear();
        cx.simulate_resize(size(px(width), px(800.)));
        cx.run_until_parked();
        cx.refresh().unwrap();
        cx.run_until_parked();

        let captured = probe.borrow().clone();
        let group = |name: &str| {
            captured
                .iter()
                .find(|(key, _)| *key == name)
                .unwrap_or_else(|| panic!("{name} was not laid out at width {width}"))
                .1
        };

        let right = group("right");
        let right_edge = f32::from(right.origin.x + right.size.width);
        assert!(
            right_edge <= width,
            "at window width {width} the right-hand controls end at {right_edge}",
        );

        let tabs = group("tabs");
        assert!(
            f32::from(tabs.size.width) >= TAB_STRIP_MIN_WIDTH,
            "at window width {width} the tab strip collapsed to {}",
            f32::from(tabs.size.width),
        );
    }
}

/// Each click walks to the next ready tab and wraps, so a set of them is
/// cleared in order rather than the same one being reopened.
#[test]
fn ready_tab_search_wraps_past_the_active_tab() {
    use crate::ui::shell::workspaces::next_ready_position;

    let ready = [true, false, true, false];

    assert_eq!(next_ready_position(&ready, 0), Some(2));
    assert_eq!(next_ready_position(&ready, 2), Some(0));
    assert_eq!(next_ready_position(&ready, 3), Some(0));

    // The active tab is the last slot visited, so its own mark still counts
    // when nothing else carries one.
    assert_eq!(next_ready_position(&[true], 0), Some(0));

    assert_eq!(next_ready_position(&[false, false], 0), None);
    assert_eq!(next_ready_position(&[], 0), None);
}
