use gpui::{AppContext, TestAppContext};
use gpui_component::resizable::ResizableState;

use crate::pane_tree::*;

type Tree = PaneTree<u32>;

/// Leaf ids double as pane payloads for easy assertions.
fn leaf_ids(tree: &Tree) -> Vec<u64> {
    tree.leaves().into_iter().map(|(id, _)| id.0).collect()
}

#[gpui::test]
fn split_right_wraps_root_leaf_and_focuses_new(cx: &mut TestAppContext) {
    let state10 = cx.new(|_| ResizableState::default());

    let mut tree = Tree::new_leaf(PaneId(1), 1);

    let outcome = tree.split(PaneId(2), 2, SplitDirection::Right, || state10.clone());

    assert!(matches!(outcome, SplitOutcome::Wrapped));
    assert_eq!(leaf_ids(&tree), vec![1, 2]);
    assert_eq!(tree.focused(), PaneId(2));
    assert!(!tree.is_single_leaf());
}

#[gpui::test]
fn split_left_puts_new_leaf_first(cx: &mut TestAppContext) {
    let state10 = cx.new(|_| ResizableState::default());

    let mut tree = Tree::new_leaf(PaneId(1), 1);

    tree.split(PaneId(2), 2, SplitDirection::Left, || state10.clone());

    assert_eq!(leaf_ids(&tree), vec![2, 1]);
}

#[gpui::test]
fn same_axis_split_inserts_sibling(cx: &mut TestAppContext) {
    let state10 = cx.new(|_| ResizableState::default());
    let state11 = cx.new(|_| ResizableState::default());

    let mut tree = Tree::new_leaf(PaneId(1), 1);

    tree.split(PaneId(2), 2, SplitDirection::Right, || state10.clone());

    tree.set_focused(PaneId(1));

    let outcome = tree.split(PaneId(3), 3, SplitDirection::Right, || state11.clone());

    // Sibling insert into the existing horizontal split, after leaf 1.
    let SplitOutcome::Inserted {
        state,
        index,
        before,
    } = outcome
    else {
        panic!("expected sibling insert");
    };

    assert_eq!((state, index, before), (state10, 0, false));
    assert_eq!(leaf_ids(&tree), vec![1, 3, 2]);
}

#[gpui::test]
fn cross_axis_split_wraps_the_leaf(cx: &mut TestAppContext) {
    let state10 = cx.new(|_| ResizableState::default());
    let state11 = cx.new(|_| ResizableState::default());

    let mut tree = Tree::new_leaf(PaneId(1), 1);

    tree.split(PaneId(2), 2, SplitDirection::Right, || state10.clone());

    tree.set_focused(PaneId(1));

    let outcome = tree.split(PaneId(3), 3, SplitDirection::Down, || state11.clone());

    assert!(matches!(outcome, SplitOutcome::Wrapped));

    // Leaf 1 became a vertical split [1, 3] nested in the horizontal root.
    assert_eq!(leaf_ids(&tree), vec![1, 3, 2]);

    let PaneNode::Split { children, .. } = tree.root() else {
        panic!("root is a split");
    };

    assert!(matches!(
        &children[0],
        PaneNode::Split {
            axis: Axis::Vertical,
            ..
        }
    ));
}

#[test]
fn remove_refuses_last_leaf() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);

    assert!(tree.remove(PaneId(1)).is_none());
}

#[gpui::test]
fn remove_collapses_two_child_split(cx: &mut TestAppContext) {
    let state10 = cx.new(|_| ResizableState::default());

    let mut tree = Tree::new_leaf(PaneId(1), 1);

    tree.split(PaneId(2), 2, SplitDirection::Right, || state10.clone());

    let (pane, outcome) = tree.remove(PaneId(2)).expect("removable");

    assert_eq!(pane, 2);
    assert!(matches!(outcome, RemoveOutcome::Collapsed));
    assert!(tree.is_single_leaf());
    assert_eq!(tree.focused(), PaneId(1));
}

#[gpui::test]
fn remove_from_wider_split_reports_index(cx: &mut TestAppContext) {
    let state10 = cx.new(|_| ResizableState::default());
    let state11 = cx.new(|_| ResizableState::default());

    let mut tree = Tree::new_leaf(PaneId(1), 1);

    tree.split(PaneId(2), 2, SplitDirection::Right, || state10.clone());

    tree.split(PaneId(3), 3, SplitDirection::Right, || state11.clone());

    // Row is [1, 2, 3]; remove the middle.
    let (_, outcome) = tree.remove(PaneId(2)).expect("removable");

    let RemoveOutcome::RemovedFromSplit { state, index } = outcome else {
        panic!("split still has two children");
    };

    assert_eq!((state, index), (state10, 1));
    assert_eq!(leaf_ids(&tree), vec![1, 3]);
}

#[gpui::test]
fn remove_collapses_nested_split_and_refocuses(cx: &mut TestAppContext) {
    let state10 = cx.new(|_| ResizableState::default());
    let state11 = cx.new(|_| ResizableState::default());

    let mut tree = Tree::new_leaf(PaneId(1), 1);

    tree.split(PaneId(2), 2, SplitDirection::Right, || state10.clone());

    tree.split(PaneId(3), 3, SplitDirection::Down, || state11.clone());

    // Root: h[1, v[2, 3]]; removing 3 collapses the nested split.
    assert_eq!(tree.focused(), PaneId(3));

    tree.remove(PaneId(3)).expect("removable");

    assert_eq!(leaf_ids(&tree), vec![1, 2]);

    // Focus fell back to an existing leaf.
    assert_eq!(tree.focused(), PaneId(1));

    let PaneNode::Split { children, .. } = tree.root() else {
        panic!("root is a split");
    };

    assert_eq!(children.len(), 2);
    assert!(children.iter().all(|c| matches!(c, PaneNode::Leaf { .. })));
}

#[gpui::test]
fn focus_only_moves_to_existing_leaves(cx: &mut TestAppContext) {
    let state10 = cx.new(|_| ResizableState::default());

    let mut tree = Tree::new_leaf(PaneId(1), 1);

    tree.split(PaneId(2), 2, SplitDirection::Right, || state10.clone());

    assert!(tree.set_focused(PaneId(1)));
    assert_eq!(tree.focused(), PaneId(1));
    assert!(!tree.set_focused(PaneId(9)));
    assert_eq!(tree.focused(), PaneId(1));
}

#[gpui::test]
fn resize_split_finds_nearest_matching_axis(cx: &mut TestAppContext) {
    let state10 = cx.new(|_| ResizableState::default());
    let state11 = cx.new(|_| ResizableState::default());

    let mut tree = Tree::new_leaf(PaneId(1), 1);

    tree.split(PaneId(2), 2, SplitDirection::Right, || state10.clone());

    tree.split(PaneId(3), 3, SplitDirection::Down, || state11.clone());

    // Focused leaf 3 sits in v-split 11 (index 1 of 2) inside h-split 10.
    assert_eq!(tree.resize_split(Axis::Vertical), Some((state11, 1, 2)));

    // The horizontal match is the root split; the focused subtree is its
    // second child.
    assert_eq!(tree.resize_split(Axis::Horizontal), Some((state10, 1, 2)));

    tree.set_focused(PaneId(1));

    assert_eq!(tree.resize_split(Axis::Vertical), None);
}
