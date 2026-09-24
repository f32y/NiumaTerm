use std::sync::{Arc, Mutex};

use gpui::{
    AppContext as _, Context, Entity, IntoElement, Modifiers, MouseButton, MouseDownEvent,
    MouseUpEvent, ParentElement as _, Render, SharedString, Styled as _, TestAppContext,
    VisualTestContext, Window, div, point, px, size,
};

use crate::TextSelectionLayer;
use crate::text::inline::InlineState;
use crate::text::inline_flow::{
    InlineFlowItem, MeasureItem, PositionedFragment, decorate_links, layout_flow,
};
use crate::text::node::LinkMark;
use crate::text::{SelectionFormat, TextView, TextViewState};

struct LinkIconRoot {
    state: Entity<TextViewState>,
    clicked: Arc<Mutex<Vec<SharedString>>>,
    format: SelectionFormat,
}

impl Render for LinkIconRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let clicked = self.clicked.clone();
        div().w(px(120.)).child(TextSelectionLayer).child(
            TextView::new(&self.state)
                .selection_format(self.format)
                .link_icon(|target| (!target.starts_with("https:")).then(|| "test.svg".into()))
                .on_link_click(move |url, _, _, _| clicked.lock().unwrap().push(url.clone())),
        )
    }
}

#[gpui::test]
fn link_icon_multi_click_selects_across_visual_fragments(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let (root, cx) = cx.add_window_view(|_, cx| LinkIconRoot {
        state: cx
            .new(|cx| TextViewState::markdown("[very_long_file_name](src/main.rs) suffix", cx)),
        clicked: Arc::default(),
        format: SelectionFormat::Plain,
    });
    let cx: &mut VisualTestContext = cx;
    cx.run_until_parked();
    for (click_count, expected) in [
        (2, "very_long_file_name"),
        (3, "very_long_file_name suffix"),
    ] {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let position = point(px(30.), px(10.));
        cx.simulate_event(MouseDownEvent {
            position,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            click_count,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            click_count,
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        root.read_with(cx, |root, cx| {
            assert_eq!(root.state.read(cx).selected_text().trim(), expected)
        });
    }
}

#[gpui::test]
fn link_icon_and_label_open_the_same_target(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let clicked = Arc::new(Mutex::new(Vec::new()));
    let (_, cx) = cx.add_window_view(|_, cx| LinkIconRoot {
        state: cx.new(|cx| TextViewState::markdown("[file](src/main.rs:7)", cx)),
        clicked: clicked.clone(),
        format: SelectionFormat::Plain,
    });
    let cx: &mut VisualTestContext = cx;
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    for (x, button) in [
        (5., MouseButton::Left),
        (30., MouseButton::Left),
        (5., MouseButton::Middle),
    ] {
        let position = point(px(x), px(10.));
        cx.simulate_mouse_down(position, button, Modifiers::default());
        cx.simulate_mouse_up(position, button, Modifiers::default());
    }
    assert_eq!(
        *clicked.lock().unwrap(),
        vec![SharedString::from("src/main.rs:7"); 3]
    );
}

#[gpui::test]
fn link_icon_drag_copies_wrapped_text_and_markdown_without_decoration(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let markdown = "Start [文件_very_long_name.rs](src/main.rs) and [config.yaml][settings] end.\n\n[settings]: config.yaml";
    let (root, cx) = cx.add_window_view(|_, cx| LinkIconRoot {
        state: cx.new(|cx| TextViewState::markdown(markdown, cx)),
        clicked: Arc::default(),
        format: SelectionFormat::Plain,
    });
    let cx: &mut VisualTestContext = cx;
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_down(
        point(px(1.), px(8.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_move(
        point(px(119.), px(250.)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_up(
        point(px(119.), px(250.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    root.read_with(cx, |root, cx| {
        assert_eq!(
            root.state.read(cx).selected_text().trim(),
            "Start 文件_very_long_name.rs and config.yaml end."
        );
        assert!(root.clicked.lock().unwrap().is_empty());
    });
    root.update(cx, |root, cx| {
        root.format = SelectionFormat::Source;
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    root.read_with(cx, |root, cx| {
        let copied = root.state.read(cx).selected_text();
        assert!(
            copied.contains("[文件_very_long_name.rs](src/main.rs)"),
            "{copied}"
        );
        assert!(copied.contains("config.yaml"), "{copied}");
        assert!(!copied.contains("test.svg"));
    });
}

#[gpui::test]
fn link_icon_wraps_with_the_first_unicode_character(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let (_, cx) = cx.add_window_view(|_, cx| LinkIconRoot {
        state: cx.new(|cx| TextViewState::markdown("", cx)),
        clicked: Arc::default(),
        format: SelectionFormat::Plain,
    });
    let cx: &mut VisualTestContext = cx;
    cx.update(|window, _| {
        let text: SharedString = "prefix 文件.rs suffix".into();
        let state = Arc::new(Mutex::new(InlineState::default()));
        state.lock().unwrap().set_text(text.clone());
        let items = decorate_links(
            vec![InlineFlowItem::Text {
                state,
                source_offset: 0,
                text,
                links: vec![(
                    7..16,
                    LinkMark {
                        url: "src/file.rs".into(),
                        ..Default::default()
                    },
                )],
                highlights: vec![],
            }],
            &|_| Some("test.svg".into()),
            gpui::black(),
        );
        let measures = items.iter().map(MeasureItem::from).collect::<Vec<_>>();
        let sizes = vec![None, Some(size(px(20.), px(16.))), None];
        for width in [30., 60., 90., 120., 180.] {
            let layout = layout_flow(
                &measures,
                &sizes,
                &window.text_style(),
                Some(px(width)),
                window,
            );
            let icon_index = layout
                .fragments
                .iter()
                .position(|fragment| {
                    matches!(fragment, PositionedFragment::Image { item_ix: 1, .. })
                })
                .unwrap();
            let PositionedFragment::Image {
                origin: icon_origin,
                ..
            } = &layout.fragments[icon_index]
            else {
                unreachable!()
            };
            let PositionedFragment::Text { origin, text, .. } = &layout.fragments[icon_index + 1]
            else {
                panic!("icon must be followed by text")
            };
            assert!(text.starts_with('文'));
            assert!((origin.y - icon_origin.y).abs() < window.line_height() / 2.);
        }
    });
}
