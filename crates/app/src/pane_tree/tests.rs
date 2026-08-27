use crate::pane_tree::*;

type Tree = PaneTree<u32, u8>;

/// Leaf ids double as pane payloads for easy assertions.
fn leaf_ids(tree: &Tree) -> Vec<u64> {
    tree.leaves().into_iter().map(|(id, _)| id.0).collect()
}

#[test]
fn split_right_wraps_root_leaf_and_focuses_new() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);
    let outcome = tree.split(PaneId(2), 2, SplitDirection::Right, || 10);
    assert!(matches!(outcome, SplitOutcome::Wrapped));
    assert_eq!(leaf_ids(&tree), vec![1, 2]);
    assert_eq!(tree.focused(), PaneId(2));
    assert!(!tree.is_single_leaf());
}

#[test]
fn split_left_puts_new_leaf_first() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);
    tree.split(PaneId(2), 2, SplitDirection::Left, || 10);
    assert_eq!(leaf_ids(&tree), vec![2, 1]);
}

#[test]
fn same_axis_split_inserts_sibling() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);
    tree.split(PaneId(2), 2, SplitDirection::Right, || 10);
    tree.set_focused(PaneId(1));
    let outcome = tree.split(PaneId(3), 3, SplitDirection::Right, || 11);
    // Sibling insert into the existing horizontal split, after leaf 1.
    let SplitOutcome::Inserted {
        state,
        index,
        before,
    } = outcome
    else {
        panic!("expected sibling insert");
    };
    assert_eq!((state, index, before), (10, 0, false));
    assert_eq!(leaf_ids(&tree), vec![1, 3, 2]);
}

#[test]
fn cross_axis_split_wraps_the_leaf() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);
    tree.split(PaneId(2), 2, SplitDirection::Right, || 10);
    tree.set_focused(PaneId(1));
    let outcome = tree.split(PaneId(3), 3, SplitDirection::Down, || 11);
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

#[test]
fn remove_collapses_two_child_split() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);
    tree.split(PaneId(2), 2, SplitDirection::Right, || 10);
    let (pane, outcome) = tree.remove(PaneId(2)).expect("removable");
    assert_eq!(pane, 2);
    assert!(matches!(outcome, RemoveOutcome::Collapsed));
    assert!(tree.is_single_leaf());
    assert_eq!(tree.focused(), PaneId(1));
}

#[test]
fn remove_from_wider_split_reports_index() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);
    tree.split(PaneId(2), 2, SplitDirection::Right, || 10);
    tree.split(PaneId(3), 3, SplitDirection::Right, || 11);
    // Row is [1, 2, 3]; remove the middle.
    let (_, outcome) = tree.remove(PaneId(2)).expect("removable");
    let RemoveOutcome::RemovedFromSplit { state, index } = outcome else {
        panic!("split still has two children");
    };
    assert_eq!((state, index), (10, 1));
    assert_eq!(leaf_ids(&tree), vec![1, 3]);
}

#[test]
fn remove_collapses_nested_split_and_refocuses() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);
    tree.split(PaneId(2), 2, SplitDirection::Right, || 10);
    tree.split(PaneId(3), 3, SplitDirection::Down, || 11);
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

#[test]
fn focus_only_moves_to_existing_leaves() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);
    tree.split(PaneId(2), 2, SplitDirection::Right, || 10);
    assert!(tree.set_focused(PaneId(1)));
    assert_eq!(tree.focused(), PaneId(1));
    assert!(!tree.set_focused(PaneId(9)));
    assert_eq!(tree.focused(), PaneId(1));
}

#[test]
fn resize_split_finds_nearest_matching_axis() {
    let mut tree = Tree::new_leaf(PaneId(1), 1);
    tree.split(PaneId(2), 2, SplitDirection::Right, || 10);
    tree.split(PaneId(3), 3, SplitDirection::Down, || 11);
    // Focused leaf 3 sits in v-split 11 (index 1 of 2) inside h-split 10.
    assert_eq!(tree.resize_split(Axis::Vertical), Some((11, 1, 2)));
    // The horizontal match is the root split; the focused subtree is its
    // second child.
    assert_eq!(tree.resize_split(Axis::Horizontal), Some((10, 1, 2)));
    tree.set_focused(PaneId(1));
    assert_eq!(tree.resize_split(Axis::Vertical), None);
}
