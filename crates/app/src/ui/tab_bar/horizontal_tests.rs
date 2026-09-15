use std::cell::Cell;
use std::rc::Rc;

use gpui::px;
use nmt_config::appearance::TabShape;

use crate::ui::platform_style::{Host, PlatformStyle as _};
use crate::ui::tab_bar::horizontal::{
    AgentTabIndicator, MIN_AUTO_TAB_WIDTH, NEW_TAB_BUTTON_WIDTH, TabDensity, agent_tab_indicator,
    auto_tab_width, progress_bar_width, shell_tab, tab_density, tab_gap,
};

struct TabGestureProbe {
    parent_presses: Rc<Cell<usize>>,
}

impl gpui::Render for TabGestureProbe {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use crate::ui::tab_bar::drag::{DragLabelPreview, DragStyle, TabDrag};
        use gpui::prelude::*;
        use gpui::{MouseButton, div};

        let presses = self.parent_presses.clone();

        div()
            .size_full()
            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                presses.set(presses.get() + 1);
            })
            .child(
                shell_tab(TabShape::Attached)
                    .w(px(160.))
                    .label("Codex")
                    .on_drag(TabDrag { from: 0 }, |_, _, _, cx| {
                        cx.new(|_| DragLabelPreview {
                            style: DragStyle::Tab,
                            label: "Codex".into(),
                            width: 160.,
                        })
                    }),
            )
    }
}

#[gpui::test]
fn tab_press_stays_out_of_titlebar_while_reorder_drag_still_starts(cx: &mut gpui::TestAppContext) {
    use gpui::{Modifiers, MouseButton, VisualTestContext, point};
    use std::cell::Cell;
    use std::rc::Rc;

    cx.update(gpui_component::init);

    let parent_presses = Rc::new(Cell::new(0));

    let handle = cx.add_window({
        let parent_presses = parent_presses.clone();

        move |_, _| TabGestureProbe { parent_presses }
    });

    let mut cx = VisualTestContext::from_window(handle.into(), cx);

    cx.refresh().unwrap();

    cx.simulate_mouse_down(
        point(px(70.), px(15.)),
        MouseButton::Left,
        Modifiers::default(),
    );

    assert_eq!(parent_presses.get(), 0);

    cx.simulate_mouse_move(
        point(px(90.), px(15.)),
        MouseButton::Left,
        Modifiers::default(),
    );

    assert!(cx.update(|_, cx| cx.has_active_drag()));
    assert_eq!(parent_presses.get(), 0);

    cx.simulate_mouse_up(
        point(px(90.), px(15.)),
        MouseButton::Left,
        Modifiers::default(),
    );

    cx.simulate_click(point(px(220.), px(15.)), Modifiers::default());

    assert_eq!(parent_presses.get(), 1);
}

#[test]
fn progress_bar_stops_at_rounded_tab_edges() {
    let tab_width = px(150.0);
    let bar_width = progress_bar_width(tab_width);

    assert_eq!(bar_width, px(134.0));
}

/// Auto Size holds the configured width while the row has room, shares the
/// row once it does not, and stops at the width that still shows the leading
/// icon. It also survives a strip that has not been measured yet.
#[test]
fn auto_size_shares_the_strip_between_tabs() {
    for shape in [TabShape::Rounded, TabShape::Attached] {
        let configured = 120.0;
        let width = |strip: f32, count: usize| auto_tab_width(strip, count, configured, shape);

        assert_eq!(width(1200.0, 2), configured);

        let crowded = width(800.0, 8);

        assert!(
            crowded < configured,
            "{crowded} should be under {configured}"
        );
        assert!(crowded > MIN_AUTO_TAB_WIDTH);

        let gap = tab_gap(shape);

        assert!(
            (crowded * 8.0 + gap * 8.0 + gap + NEW_TAB_BUTTON_WIDTH - 800.0).abs() < 0.001,
            "the tabs and their gaps should consume the strip exactly with {shape:?}",
        );

        assert_eq!(width(800.0, 40), MIN_AUTO_TAB_WIDTH);
        assert_eq!(width(0.0, 8), configured);
        assert_eq!(width(1200.0, 0), configured);
        assert_eq!(auto_tab_width(800.0, 40, 30.0, shape), 30.0);
    }
}

#[test]
fn a_shrinking_tab_gives_up_the_title_first() {
    assert_eq!(tab_density(120.0), TabDensity::Full);
    assert_eq!(tab_density(Host::FULL_TAB_WIDTH), TabDensity::Full);
    assert_eq!(tab_density(Host::FULL_TAB_WIDTH - 1.0), TabDensity::Compact);
    assert_eq!(tab_density(Host::COMPACT_TAB_WIDTH), TabDensity::Compact);
    assert_eq!(
        tab_density(Host::COMPACT_TAB_WIDTH - 1.0),
        TabDensity::IconOnly
    );

    const { assert!(MIN_AUTO_TAB_WIDTH < Host::COMPACT_TAB_WIDTH) };

    assert_eq!(tab_density(MIN_AUTO_TAB_WIDTH), TabDensity::IconOnly);
}

#[test]
fn busy_indicator_takes_precedence_over_ready() {
    assert_eq!(
        agent_tab_indicator(true, true),
        Some(AgentTabIndicator::Busy)
    );
    assert_eq!(
        agent_tab_indicator(false, true),
        Some(AgentTabIndicator::Ready)
    );
    assert_eq!(agent_tab_indicator(false, false), None);
}
