use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, Modifiers, MouseButton,
    ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, Window, div, point,
    px,
};

use crate::text::{SelectionFormat, TextView, TextViewState};
use crate::{TextSelection, TextSelectionLayer, v_flex};

struct FragmentRoot {
    parent: Entity<TextViewState>,
    first: Entity<TextViewState>,
    last: Entity<TextViewState>,
    format: SelectionFormat,
    show_first: bool,
}

impl Render for FragmentRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w(px(400.))
            .child(TextSelectionLayer)
            .child(
                div()
                    .debug_selector(|| "first".into())
                    .child(if self.show_first {
                        TextView::new(&self.first)
                            .selection_format(self.format)
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "last".into())
                    .child(TextView::new(&self.last).selection_format(self.format)),
            )
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn fragments_copy_unrendered_blocks_and_select_all_source(cx: &mut TestAppContext) {
    cx.update(crate::init);

    let source =
        "First paragraph.\n\n**Hidden [file][ref].**\n\nLast paragraph.\n\n[ref]: src/main.rs";

    let (root, cx) = cx.add_window_view(|_, cx| {
        let parent = cx.new(|cx| TextViewState::markdown(source, cx));
        let fragments = parent.update(cx, |parent, _| parent.fragments());

        assert_eq!(fragments.len(), 4);

        FragmentRoot {
            parent,
            first: cx.new(|cx| TextViewState::from_fragment(fragments[0].clone(), cx)),
            last: cx.new(|cx| TextViewState::from_fragment(fragments[2].clone(), cx)),
            format: SelectionFormat::Source,
            show_first: true,
        }
    });

    let cx: &mut VisualTestContext = cx;

    cx.run_until_parked();

    draw(cx);

    let first = cx.debug_bounds("first").unwrap();
    let last = cx.debug_bounds("last").unwrap();
    let start = point(first.left() + px(1.), first.top() + px(8.));
    let end = point(last.right() - px(1.), last.bottom() - px(1.));

    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());

    draw(cx);

    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());

    draw(cx);

    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());

    draw(cx);

    let copied = cx.update(TextSelection::selected_text);

    assert_eq!(
        copied,
        "First paragraph.\n\n**Hidden [file][ref].**\n\nLast paragraph."
    );

    cx.update(TextSelection::clear);
    cx.simulate_mouse_down(end, MouseButton::Left, Modifiers::default());

    draw(cx);

    cx.simulate_mouse_move(start, Some(MouseButton::Left), Modifiers::default());

    draw(cx);

    cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::default());

    draw(cx);

    assert_eq!(cx.update(TextSelection::selected_text), copied);

    cx.update(TextSelection::clear);

    let partial_start = point(first.left() + px(8.), first.top() + px(8.));
    let partial_end = point(last.left() + px(8.), last.top() + px(8.));

    cx.simulate_mouse_down(partial_start, MouseButton::Left, Modifiers::default());

    draw(cx);

    cx.simulate_mouse_move(partial_end, Some(MouseButton::Left), Modifiers::default());

    draw(cx);

    cx.simulate_mouse_up(partial_end, MouseButton::Left, Modifiers::default());

    draw(cx);

    let partial_copy = cx.update(TextSelection::selected_text);

    assert!(partial_copy.contains("Hidden [file][ref]"));

    root.update(cx, |root, cx| {
        root.show_first = false;

        cx.notify();
    });

    draw(cx);
    draw(cx);

    assert_eq!(cx.update(TextSelection::selected_text), partial_copy);

    root.update(cx, |root, cx| {
        root.format = SelectionFormat::Plain;

        cx.notify();
    });

    draw(cx);

    let plain = cx.update(TextSelection::selected_text);

    assert_eq!(plain.matches("Hidden file.").count(), 1);
    assert!(!plain.contains("**"));

    root.update(cx, |root, cx| {
        root.format = SelectionFormat::Source;

        cx.notify();
    });

    draw(cx);

    root.update(cx, |root, cx| {
        root.show_first = true;

        cx.notify();
    });

    draw(cx);

    let first = root.read_with(cx, |root, _| root.first.clone());

    cx.update(TextSelection::clear);
    first.update(cx, |state, cx| state.select_all(cx));

    draw(cx);

    assert_eq!(cx.update(TextSelection::selected_text), source);

    root.read_with(cx, |root, cx| {
        assert!(root.first.read(cx).retains_selection_state(cx));
        assert!(!root.last.read(cx).retains_selection_state(cx));
    });

    root.update(cx, |root, cx| {
        root.show_first = false;

        cx.notify();
    });

    draw(cx);
    draw(cx);

    assert_eq!(cx.update(TextSelection::selected_text), source);

    let parent = root.read_with(cx, |root, _| root.parent.clone());

    parent.update(cx, |state, cx| {
        state.set_text("A replacement document.", cx)
    });

    cx.run_until_parked();

    draw(cx);

    assert_eq!(cx.update(TextSelection::selected_text), "");
}

#[gpui::test]
fn fragment_views_release_document_snapshots_and_refresh_reference_context(
    cx: &mut TestAppContext,
) {
    cx.update(crate::init);

    let parent = cx.new(|cx| TextViewState::markdown("First [file][ref].\n\nTail.", cx));
    let fragments = parent.update(cx, |state, _| state.fragments());
    let snapshot = Arc::downgrade(&fragments[0].content);
    let original_key = fragments[0].key();
    let child = cx.new(|cx| TextViewState::from_fragment(fragments[0].clone(), cx));

    drop(fragments);

    assert!(snapshot.upgrade().is_none());

    parent.update(cx, |state, cx| state.push_str("\n\n[ref]: src/main.rs", cx));
    cx.run_until_parked();

    let fragments = parent.update(cx, |state, _| state.fragments());
    let snapshot = Arc::downgrade(&fragments[0].content);

    assert_ne!(fragments[0].key(), original_key);

    child.update(cx, |state, cx| state.set_fragment(fragments[0].clone(), cx));

    drop(fragments);

    assert!(snapshot.upgrade().is_none());

    child.read_with(cx, |state, _| {
        assert_eq!(state.source(), "First [file][ref].");
        assert!(state.parsed_content.document.source.is_empty());
        assert!(state.parsed_content.node_cx.link_refs.contains_key("ref"));
    });
}
