//! Per-tab split-pane tree: leaves are terminal panes, splits are resizable
//! groups with one axis each (same-axis children flatten into siblings, so the
//! tree stays shallow, tmux-style). Pure logic — generic over the leaf pane
//! type `L` and the split-state handle `S` so it unit-tests without GPUI.
//!
//! Invariant: a `PaneTree` always has at least one leaf, and `focused` always
//! names an existing leaf. `remove` refuses the last leaf.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui::Axis;

/// Stable per-pane identity (same monotonic id source as tabs/workspaces).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

/// Direction of a split/resize action, from the arrow key that triggered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    Up,
    Down,
    Left,
    Right,
}

impl SplitDirection {
    /// Left/Right split side-by-side (horizontal group); Up/Down stack
    /// (vertical group).
    pub fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }

    /// True when the new pane goes before the focused one (Left/Up).
    pub fn before(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }

    /// True for the axis-positive arrows (Right/Down).
    pub fn positive(self) -> bool {
        matches!(self, Self::Right | Self::Down)
    }
}

/// Ids for split groups (stable GPUI element/state identity across renders).
static NEXT_SPLIT_ID: AtomicU64 = AtomicU64::new(1);

fn alloc_split_id() -> u64 {
    NEXT_SPLIT_ID.fetch_add(1, Ordering::Relaxed)
}

pub enum PaneNode<L, S> {
    Leaf {
        id: PaneId,
        pane: L,
    },
    Split {
        /// Stable id for the GPUI element / keyed state of this group.
        id: u64,
        axis: Axis,
        children: Vec<PaneNode<L, S>>,
        /// The resizable group's size/drag state handle.
        state: S,
        /// Saved size ratios awaiting application once the group has real
        /// bounds (session restore); cleared after applying.
        pending_ratios: Option<Vec<f32>>,
    },
}

impl<L, S> PaneNode<L, S> {
    fn first_leaf_id(&self) -> PaneId {
        match self {
            Self::Leaf { id, .. } => *id,
            Self::Split { children, .. } => children[0].first_leaf_id(),
        }
    }

    fn contains(&self, id: PaneId) -> bool {
        match self {
            Self::Leaf { id: leaf, .. } => *leaf == id,
            Self::Split { children, .. } => children.iter().any(|c| c.contains(id)),
        }
    }
}

/// What the caller must do to the split-state handle after a [`PaneTree::split`].
pub enum SplitOutcome<S> {
    /// The new leaf was inserted as a sibling at `index` in an existing
    /// same-axis split: halve the panel at the focused index into it
    /// (`ResizableState::split_panel`).
    Inserted {
        state: S,
        index: usize,
        before: bool,
    },
    /// The focused leaf was wrapped in a fresh two-child split; the fresh
    /// state lays out 50/50 on its own.
    Wrapped,
}

/// What the caller must do after a [`PaneTree::remove`].
pub enum RemoveOutcome<S> {
    /// Removed from a split that still has 2+ children: call
    /// `ResizableState::remove_panel(index)` on `state`.
    RemovedFromSplit { state: S, index: usize },
    /// The parent split collapsed into its surviving child; its state handle
    /// was dropped with it — nothing to fix up.
    Collapsed,
}

pub struct PaneTree<L, S> {
    root: PaneNode<L, S>,
    focused: PaneId,
}

impl<L, S: Clone> PaneTree<L, S> {
    pub fn new_leaf(id: PaneId, pane: L) -> Self {
        Self {
            root: PaneNode::Leaf { id, pane },
            focused: id,
        }
    }

    /// Build from a restored root; focus falls to the first leaf.
    pub fn from_root(root: PaneNode<L, S>) -> Self {
        let focused = root.first_leaf_id();
        Self { root, focused }
    }

    pub fn root(&self) -> &PaneNode<L, S> {
        &self.root
    }

    pub fn focused(&self) -> PaneId {
        self.focused
    }

    /// Focus `id` if it names an existing leaf; returns whether it did.
    pub fn set_focused(&mut self, id: PaneId) -> bool {
        let found = self.root.contains(id);
        if found {
            self.focused = id;
        }
        found
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.root.contains(id)
    }

    pub fn focused_pane(&self) -> &L {
        self.leaves()
            .into_iter()
            .find(|(id, _)| *id == self.focused)
            .map(|(_, pane)| pane)
            .expect("focused always names an existing leaf")
    }

    /// All leaves in layout order.
    pub fn leaves(&self) -> Vec<(PaneId, &L)> {
        fn walk<'a, L, S>(node: &'a PaneNode<L, S>, out: &mut Vec<(PaneId, &'a L)>) {
            match node {
                PaneNode::Leaf { id, pane, .. } => out.push((*id, pane)),
                PaneNode::Split { children, .. } => {
                    children.iter().for_each(|c| walk(c, out));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    pub fn is_single_leaf(&self) -> bool {
        matches!(self.root, PaneNode::Leaf { .. })
    }

    /// Split the focused leaf in `direction`, inserting `new_pane`. Same-axis
    /// parent → sibling insert (shallow tree); otherwise the leaf is wrapped in
    /// a fresh two-child split built with `make_state`. The new leaf becomes
    /// focused. Returns what the caller must apply to the resizable state.
    pub fn split(
        &mut self,
        new_id: PaneId,
        new_pane: L,
        direction: SplitDirection,
        make_state: impl FnOnce() -> S,
    ) -> SplitOutcome<S> {
        let axis = direction.axis();
        let before = direction.before();
        let focused = self.focused;
        let outcome = Self::split_at(
            &mut self.root,
            focused,
            new_id,
            new_pane,
            axis,
            before,
            make_state,
        )
        .expect("focused always names an existing leaf");
        self.focused = new_id;
        outcome
    }

    fn split_at(
        node: &mut PaneNode<L, S>,
        at: PaneId,
        new_id: PaneId,
        new_pane: L,
        axis: Axis,
        before: bool,
        make_state: impl FnOnce() -> S,
    ) -> Option<SplitOutcome<S>> {
        // Same-axis parent: insert the new leaf as a direct sibling.
        if let PaneNode::Split {
            axis: split_axis,
            children,
            state,
            ..
        } = node
            && *split_axis == axis
            && let Some(index) = children
                .iter()
                .position(|c| matches!(c, PaneNode::Leaf { id, .. } if *id == at))
        {
            let insert_at = if before { index } else { index + 1 };
            children.insert(
                insert_at,
                PaneNode::Leaf {
                    id: new_id,
                    pane: new_pane,
                },
            );
            return Some(SplitOutcome::Inserted {
                state: state.clone(),
                index,
                before,
            });
        }
        match node {
            PaneNode::Leaf { id, .. } if *id == at => {
                // Wrap the leaf in a fresh split on the requested axis.
                let old = std::mem::replace(
                    node,
                    PaneNode::Split {
                        id: alloc_split_id(),
                        axis,
                        children: Vec::new(),
                        state: make_state(),
                        pending_ratios: None,
                    },
                );
                let new_leaf = PaneNode::Leaf {
                    id: new_id,
                    pane: new_pane,
                };
                let PaneNode::Split { children, .. } = node else {
                    unreachable!()
                };
                if before {
                    children.extend([new_leaf, old]);
                } else {
                    children.extend([old, new_leaf]);
                }
                Some(SplitOutcome::Wrapped)
            }
            PaneNode::Leaf { .. } => None,
            PaneNode::Split { children, .. } => {
                let mut new_pane = Some(new_pane);
                let mut make_state = Some(make_state);
                children.iter_mut().find_map(|child| {
                    if !child.contains(at) {
                        return None;
                    }
                    Self::split_at(
                        child,
                        at,
                        new_id,
                        new_pane.take().expect("at most one child contains `at`"),
                        axis,
                        before,
                        make_state.take().expect("at most one child contains `at`"),
                    )
                })
            }
        }
    }

    /// Remove the leaf `id`, returning its pane (for dropping) plus the
    /// state fix-up the caller must apply. Refuses the last leaf (`None`).
    /// A split left with one child collapses into that child; focus falls to
    /// the leaf now occupying the removed leaf's neighborhood.
    pub fn remove(&mut self, id: PaneId) -> Option<(L, RemoveOutcome<S>)> {
        if self.is_single_leaf() {
            return None;
        }
        let (pane, outcome) = Self::remove_at(&mut self.root, id)?;
        // Collapse a root split reduced to a single child.
        if let PaneNode::Split { children, .. } = &mut self.root
            && children.len() == 1
        {
            self.root = children.pop().expect("len checked");
        }
        if self.focused == id {
            self.focused = self.root.first_leaf_id();
        }
        Some((pane, outcome))
    }

    fn remove_at(node: &mut PaneNode<L, S>, id: PaneId) -> Option<(L, RemoveOutcome<S>)> {
        let PaneNode::Split {
            children, state, ..
        } = node
        else {
            return None;
        };
        if let Some(index) = children
            .iter()
            .position(|c| matches!(c, PaneNode::Leaf { id: leaf, .. } if *leaf == id))
        {
            let PaneNode::Leaf { pane, .. } = children.remove(index) else {
                unreachable!()
            };
            let outcome = if children.len() == 1 {
                RemoveOutcome::Collapsed
            } else {
                RemoveOutcome::RemovedFromSplit {
                    state: state.clone(),
                    index,
                }
            };
            return Some((pane, outcome));
        }
        let result = children
            .iter_mut()
            .find(|c| c.contains(id))
            .and_then(|c| Self::remove_at(c, id))?;
        // Collapse any child split reduced to a single child.
        for child in children.iter_mut() {
            if let PaneNode::Split {
                children: inner, ..
            } = child
                && inner.len() == 1
            {
                *child = inner.pop().expect("len checked");
            }
        }
        Some(result)
    }

    /// The nearest ancestor split of the focused leaf whose axis matches:
    /// `(state, child index of the focused subtree, child count)`. `None` when
    /// no matching-axis split exists (resize is a no-op then).
    pub fn resize_split(&self, axis: Axis) -> Option<(S, usize, usize)> {
        fn walk<L, S: Clone>(
            node: &PaneNode<L, S>,
            at: PaneId,
            axis: Axis,
        ) -> Option<(S, usize, usize)> {
            let PaneNode::Split {
                axis: split_axis,
                children,
                state,
                ..
            } = node
            else {
                return None;
            };
            let index = children.iter().position(|c| c.contains(at))?;
            // Deepest match wins (nearest ancestor); recurse first.
            if let Some(found) = walk(&children[index], at, axis) {
                return Some(found);
            }
            (*split_axis == axis).then(|| (state.clone(), index, children.len()))
        }
        walk(&self.root, self.focused, axis)
    }

    /// Visit every split's `(state, pending_ratios)` pair mutably (ratio
    /// application after a session restore).
    pub fn for_each_split_mut(&mut self, f: &mut impl FnMut(&S, &mut Option<Vec<f32>>)) {
        fn walk<L, S>(node: &mut PaneNode<L, S>, f: &mut impl FnMut(&S, &mut Option<Vec<f32>>)) {
            if let PaneNode::Split {
                children,
                state,
                pending_ratios,
                ..
            } = node
            {
                f(state, pending_ratios);
                children.iter_mut().for_each(|c| walk(c, f));
            }
        }
        walk(&mut self.root, f);
    }

    /// Build a split node for session restore (children already built).
    pub fn restored_split(
        axis: Axis,
        children: Vec<PaneNode<L, S>>,
        state: S,
        ratios: Option<Vec<f32>>,
    ) -> PaneNode<L, S> {
        let ratios = ratios.filter(|r| r.len() == children.len());
        PaneNode::Split {
            id: alloc_split_id(),
            axis,
            children,
            state,
            pending_ratios: ratios,
        }
    }

    /// Build a leaf node for session restore.
    pub fn restored_leaf(id: PaneId, pane: L) -> PaneNode<L, S> {
        PaneNode::Leaf { id, pane }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
