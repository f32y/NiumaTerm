use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    ParentElement as _, ScrollDelta, ScrollWheelEvent, Styled as _, TestAppContext,
    VisualTestContext, div, point, px, size,
};

use crate::setting::{
    SelectIndex, SettingGroup, SettingItem, SettingPage, Settings, SettingsState, SettingsView,
};

#[gpui::test]
fn scrolling_reuses_the_other_panel_and_data_changes_refresh_content(cx: &mut TestAppContext) {
    cx.update(crate::init);
    cx.update(|cx| cx.set_smooth_wheel_scrolling(false));
    let builds = Rc::new(Cell::new(0));
    let content_renders = Rc::new(Cell::new(0));
    let rendered_page = Rc::new(Cell::new(usize::MAX));
    let value = Rc::new(Cell::new(0));
    let rendered_value = Rc::new(Cell::new(usize::MAX));
    let mut state = None;
    let handle = cx.add_window(|window, cx| {
        let settings_state = SettingsState::owned(SelectIndex::default(), window, cx);
        state = Some(settings_state.clone());
        SettingsView::new(
            settings_state,
            {
                let builds = builds.clone();
                let content_renders = content_renders.clone();
                let rendered_page = rendered_page.clone();
                let value = value.clone();
                let rendered_value = rendered_value.clone();
                move |_| {
                    builds.set(builds.get() + 1);
                    Settings::new("panel-cache-test").pages((0..30).map(|index| {
                        let content_renders = content_renders.clone();
                        let rendered_page = rendered_page.clone();
                        let rendered_value = rendered_value.clone();
                        let value = value.get();
                        SettingPage::new(format!("Page {index}")).group(
                            SettingGroup::new().item(
                                SettingItem::render(move |_, _, _| {
                                    content_renders.set(content_renders.get() + 1);
                                    rendered_page.set(index);
                                    rendered_value.set(value);
                                    div().h(px(1600.)).child(format!("Value {value}"))
                                })
                                .keywords([format!("unique-{index}")]),
                            ),
                        )
                    }))
                }
            },
            cx,
        )
    });
    let state = state.unwrap();
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    cx.simulate_resize(size(px(1100.), px(420.)));
    cx.run_until_parked();

    let before_builds = builds.get();
    let before_content = content_renders.get();
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(100.), px(300.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-40.))),
        ..Default::default()
    });
    assert_eq!(
        builds.get(),
        before_builds + 1,
        "only navigation should rebuild"
    );
    assert_eq!(
        content_renders.get(),
        before_content,
        "content must be reused while navigation scrolls"
    );

    // Crossing between panels may redraw navigation once to clear hover state.
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(750.), px(300.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-40.))),
        ..Default::default()
    });
    let before_builds = builds.get();
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(750.), px(300.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-40.))),
        ..Default::default()
    });
    assert_eq!(
        builds.get(),
        before_builds + 1,
        "only content should rebuild"
    );
    assert!(content_renders.get() > before_content);

    let before_content = content_renders.get();
    cx.update(|_, cx| {
        let input = state.read(cx).search_input.clone();
        input.update(cx, |_, cx| cx.notify());
    });
    assert_eq!(
        content_renders.get(),
        before_content,
        "caret notifications must not rebuild unchanged search results"
    );

    cx.update(|_, cx| {
        state.update(cx, |state, cx| {
            state.select(
                SelectIndex {
                    page_ix: 1,
                    group_ix: None,
                },
                cx,
            );
        })
    });
    assert_eq!(rendered_page.get(), 1);

    value.set(42);
    handle
        .update(&mut cx, |view, _, cx| view.refresh(cx))
        .unwrap();
    assert_eq!(rendered_value.get(), 42);

    cx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.select(SelectIndex::default(), cx);
            state
                .search_input
                .update(cx, |input, cx| input.set_value("unique-5", window, cx));
        });
    });
    assert_eq!(
        rendered_page.get(),
        5,
        "search must invalidate cached content"
    );
}
