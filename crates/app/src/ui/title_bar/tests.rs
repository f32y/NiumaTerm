use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Bounds, Pixels, TestAppContext};

use crate::ui::platform_style::{Host, PlatformStyle as _};
use crate::ui::title_bar::{
    TAB_STRIP_MIN_WIDTH, title_bar_git_summary, title_bar_leading_region, title_bar_trailing_region,
};
use crate::window::MIN_WINDOW_WIDTH;

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
        use gpui_component::tab::{Tab, TabBar, TabVariant};
        use gpui_component::{ElementExt as _, IconName, Sizable as _, TitleBar, h_flex};

        use crate::ui::composition::{toolbar_button, toolbar_toggle};

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
                        title_bar_leading_region(self.1)
                            .on_prepaint(rec("left", probe.clone()))
                            .children((0..4usize).map(|index| {
                                div().flex_none().child(
                                    toolbar_button(("leading", index)).icon(IconName::Settings),
                                )
                            })),
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
                        title_bar_git_summary().child(
                            h_flex()
                                .px_2()
                                .gap_1()
                                .text_sm()
                                .child("+1234")
                                .child("-5678"),
                        ),
                    )
                    .child(
                        title_bar_trailing_region()
                            .on_prepaint(rec("right", probe.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .child(toolbar_toggle("git").icon(IconName::GitBranch)),
                            )
                            .child(
                                div().flex_none().child(
                                    toolbar_toggle("workflows").icon(IconName::Bot).child("2"),
                                ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .child(toolbar_toggle("tasks").icon(IconName::Bot).child("2")),
                            ),
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

    cx.update(gpui_component::init);

    let trailing_inset = Host::TITLE_BAR_TRAILING_INSET;

    let probe: TitleBarProbe = Default::default();

    let handle = cx.add_window({
        let probe = probe.clone();

        move |_, _| TitleBarProbeView(probe, MAX_WIDTH)
    });

    let mut cx = VisualTestContext::from_window(handle.into(), cx);

    // Also reserve the Windows caption controls and title bar padding:
    // 640 - 3 * 46 - 8 leaves 494 pixels for application content.
    for width in [1200.0f32, 900.0, 700.0, MIN_WINDOW_WIDTH, 494.0] {
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
        let right_edge: f32 = (right.origin.x + right.size.width).into();

        assert!(
            right_edge <= width - trailing_inset,
            "at window width {width} the right-hand controls end at {right_edge}, inside the reserved edge inset of {trailing_inset}",
        );

        let tabs = group("tabs");
        let tab_width: f32 = tabs.size.width.into();

        assert!(
            tab_width >= TAB_STRIP_MIN_WIDTH,
            "at window width {width} the tab strip collapsed to {}",
            tab_width,
        );
    }
}
