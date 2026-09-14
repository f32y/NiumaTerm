use gpui::{
    AnyElement, App, Axis, Context, Entity, IntoElement, ParentElement, Render, Styled,
    TestAppContext, VisualTestContext, Window, div, px,
};
use gpui_component::resizable::{ResizablePanelGroup, ResizableState, resizable_panel};

use crate::pane_tree::{PaneId, PaneNode, SplitDirection};
use crate::ui::terminal_layout::TerminalLayout;

struct LayoutView(TerminalLayout<u32>);

fn render_node(node: &PaneNode<u32>) -> AnyElement {
    match node {
        PaneNode::Leaf { .. } => div().size_full().into_any_element(),
        PaneNode::Split {
            id,
            axis,
            children,
            state,
            ..
        } => {
            let mut group = ResizablePanelGroup::new(("layout-test", *id as usize))
                .axis(*axis)
                .with_state(state);

            for child in children {
                group = group.child(resizable_panel().child(render_node(child)));
            }

            group.into_any_element()
        }
    }
}

impl Render for LayoutView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(800.))
            .h(px(600.))
            .child(render_node(self.0.tree().root()))
    }
}

fn harness(cx: &mut TestAppContext) -> (Entity<LayoutView>, &mut VisualTestContext) {
    cx.update(gpui_component::init);

    let (view, cx) = cx.add_window_view(|_, cx| {
        let mut layout = TerminalLayout::new_leaf(PaneId(1), 1);

        assert!(layout.split(PaneId(2), 2, SplitDirection::Right, cx));

        LayoutView(layout)
    });

    draw(cx);

    (view, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));

    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn root_state(layout: &TerminalLayout<u32>) -> Entity<ResizableState> {
    let PaneNode::Split { state, .. } = layout.tree().root() else {
        panic!("expected a split");
    };

    state.clone()
}

fn assert_aligned(node: &PaneNode<u32>, cx: &App) {
    if let PaneNode::Split {
        children, state, ..
    } = node
    {
        assert_eq!(children.len(), state.read(cx).sizes().len());

        for child in children {
            assert_aligned(child, cx);
        }
    }
}

#[gpui::test]
fn same_axis_split_and_removal_update_sizes_before_render(cx: &mut TestAppContext) {
    let (view, cx) = harness(cx);

    view.update(cx, |view, cx| {
        let layout = &mut view.0;
        let state = root_state(layout);

        state.update(cx, |state, cx| state.set_ratios(&[0.75, 0.25], cx));

        let original = state.read(cx).sizes().clone();

        assert!(layout.tree_mut().set_focused(PaneId(1)));
        assert!(layout.split(PaneId(3), 3, SplitDirection::Left, cx));
        assert_eq!(
            layout
                .tree()
                .leaves()
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            [PaneId(3), PaneId(1), PaneId(2)]
        );
        assert_eq!(
            state.read(cx).sizes(),
            &[original[0] / 2., original[0] / 2., original[1]]
        );
        assert_eq!(root_state(layout), state);

        assert_aligned(layout.tree().root(), cx);

        assert_eq!(layout.remove(PaneId(1), cx), Some(1));

        assert_aligned(layout.tree().root(), cx);

        assert_eq!(layout.tree().focused(), PaneId(3));
        assert_eq!(
            layout
                .tree()
                .leaves()
                .iter()
                .map(|(_, value)| **value)
                .collect::<Vec<_>>(),
            [3, 2]
        );

        let sizes = state.read(cx).sizes();

        assert!((sizes[0] / sizes[1] - 1.5).abs() < 0.001);

        assert_eq!(layout.remove(PaneId(3), cx), Some(3));
        assert!(layout.tree().is_single_leaf());
        assert_eq!(layout.tree().focused(), PaneId(2));
        assert_eq!(layout.remove(PaneId(2), cx), None);
        assert_eq!(layout.remove(PaneId(99), cx), None);
    });
}

#[gpui::test]
fn nested_split_collapse_preserves_outer_sizes_and_identity(cx: &mut TestAppContext) {
    let (view, cx) = harness(cx);

    let outer = view.update(cx, |view, cx| {
        let state = root_state(&view.0);

        assert!(view.0.split(PaneId(3), 3, SplitDirection::Down, cx));

        state
    });

    draw(cx);

    view.update(cx, |view, cx| {
        let layout = &mut view.0;
        let before = outer.read(cx).sizes().clone();

        assert_aligned(layout.tree().root(), cx);

        let PaneNode::Split { children, .. } = layout.tree().root() else {
            panic!("expected outer split");
        };

        assert!(matches!(
            &children[1],
            PaneNode::Split {
                axis: Axis::Vertical,
                ..
            }
        ));
        assert_eq!(layout.remove(PaneId(2), cx), Some(2));
        assert_eq!(root_state(layout), outer);
        assert_eq!(outer.read(cx).sizes(), &before);

        assert_aligned(layout.tree().root(), cx);

        assert_eq!(layout.tree().focused(), PaneId(3));
        assert_eq!(layout.remove(PaneId(1), cx), Some(1));
        assert_eq!(*layout.tree().focused_pane(), 3);
        assert!(layout.tree().is_single_leaf());
    });
}

#[gpui::test]
fn too_small_split_leaves_tree_focus_and_sizes_unchanged(cx: &mut TestAppContext) {
    let (view, cx) = harness(cx);

    view.update(cx, |view, cx| {
        let layout = &mut view.0;
        let state = root_state(layout);

        state.update(cx, |state, cx| state.set_ratios(&[0.85, 0.15], cx));

        let sizes = state.read(cx).sizes().clone();

        assert!(!layout.split(PaneId(3), 3, SplitDirection::Right, cx));
        assert!(!layout.tree().contains(PaneId(3)));
        assert_eq!(layout.tree().focused(), PaneId(2));
        assert_eq!(state.read(cx).sizes(), &sizes);

        assert_aligned(layout.tree().root(), cx);
    });
}

#[gpui::test]
fn mutations_before_first_render_keep_remaining_panes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let mut layout = TerminalLayout::new_leaf(PaneId(1), 1);

        assert!(layout.split(PaneId(2), 2, SplitDirection::Right, cx));
        assert!(layout.split(PaneId(3), 3, SplitDirection::Left, cx));
        assert!(root_state(&layout).read(cx).sizes().is_empty());
        assert_eq!(layout.remove(PaneId(3), cx), Some(3));
        assert_eq!(
            layout
                .tree()
                .leaves()
                .iter()
                .map(|(_, value)| **value)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(layout.remove(PaneId(1), cx), Some(1));
        assert_eq!(*layout.tree().focused_pane(), 2);
    });
}
