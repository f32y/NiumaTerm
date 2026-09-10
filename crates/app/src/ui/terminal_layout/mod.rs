//! Owns pane structure together with the matching resizable groups.

use gpui::{App, AppContext, Entity, Pixels, Window};
use gpui_component::resizable::{PANEL_MIN_SIZE, ResizableState};

use crate::pane_tree::{PaneId, PaneNode, PaneTree, RemoveOutcome, SplitDirection, SplitOutcome};

pub(crate) struct TerminalLayout<L> {
    tree: PaneTree<L, Entity<ResizableState>>,
}

impl<L> TerminalLayout<L> {
    pub(super) fn new_leaf(id: PaneId, pane: L) -> Self {
        Self {
            tree: PaneTree::new_leaf(id, pane),
        }
    }

    pub(super) fn from_root(root: PaneNode<L, Entity<ResizableState>>) -> Self {
        Self {
            tree: PaneTree::from_root(root),
        }
    }

    pub(super) fn root(&self) -> &PaneNode<L, Entity<ResizableState>> {
        self.tree.root()
    }

    pub(super) fn focused(&self) -> PaneId {
        self.tree.focused()
    }

    pub(super) fn set_focused(&mut self, id: PaneId) -> bool {
        self.tree.set_focused(id)
    }

    pub(super) fn focused_pane(&self) -> &L {
        self.tree.focused_pane()
    }

    pub(super) fn contains(&self, id: PaneId) -> bool {
        self.tree.contains(id)
    }

    pub(super) fn leaves(&self) -> Vec<(PaneId, &L)> {
        self.tree.leaves()
    }

    pub(super) fn is_single_leaf(&self) -> bool {
        self.tree.is_single_leaf()
    }

    /// Split sizes and child order change together. A group that has not been
    /// rendered has no measured slots yet; its first layout supplies them.
    pub(super) fn split(
        &mut self,
        id: PaneId,
        pane: L,
        direction: SplitDirection,
        cx: &mut App,
    ) -> bool {
        // Only an immediate same-axis parent receives another slot. A more
        // distant matching ancestor keeps its child count when a leaf wraps.
        fn parent_state<L>(
            node: &PaneNode<L, Entity<ResizableState>>,
            id: PaneId,
            direction: SplitDirection,
        ) -> Option<(Entity<ResizableState>, usize)> {
            let PaneNode::Split {
                axis,
                children,
                state,
                ..
            } = node
            else {
                return None;
            };
            for (index, child) in children.iter().enumerate() {
                if matches!(child, PaneNode::Leaf { id: child_id, .. } if *child_id == id) {
                    return (*axis == direction.axis()).then(|| (state.clone(), index));
                }
                if let Some(found) = parent_state(child, id, direction) {
                    return Some(found);
                }
            }
            None
        }

        if let Some((state, index)) = parent_state(self.tree.root(), self.tree.focused(), direction)
            && let Some(size) = state.read(cx).sizes().get(index)
            && *size / 2.0 < PANEL_MIN_SIZE
        {
            return false;
        }

        match self.tree.split(id, pane, direction, || {
            cx.new(|_| ResizableState::default())
        }) {
            SplitOutcome::Inserted {
                state,
                index,
                before,
            } => {
                state.update(cx, |state, cx| state.split_panel(index, before, cx));
            }
            SplitOutcome::Wrapped => {}
        }
        true
    }

    pub(super) fn remove(&mut self, id: PaneId, cx: &mut App) -> Option<L> {
        let (pane, outcome) = self.tree.remove(id)?;
        match outcome {
            RemoveOutcome::RemovedFromSplit { state, index } => {
                // Unrendered groups have no slots to remove. Their first
                // render creates slots from the remaining children.
                state.update(cx, |state, cx| {
                    if index < state.sizes().len() {
                        state.remove_panel(index, cx);
                    }
                });
            }
            RemoveOutcome::Collapsed => {}
        }
        Some(pane)
    }

    pub(super) fn resize(
        &self,
        direction: SplitDirection,
        step: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some((state, index, count)) = self.tree.resize_split(direction.axis()) else {
            return false;
        };
        let Some(current) = state.read(cx).sizes().get(index).copied() else {
            return false;
        };
        let grow = direction.positive() == (index + 1 < count);
        let target = if grow { current + step } else { current - step };
        state.update(cx, |state, cx| {
            state.resize_panel(index, target, window, cx)
        });
        true
    }

    pub(super) fn apply_pending_ratios(&mut self, cx: &mut App) {
        self.tree.for_each_split_mut(&mut |state, pending| {
            if let Some(ratios) = pending.take_if(|_| state.read(cx).has_bounds()) {
                state.update(cx, |state, cx| state.set_ratios(&ratios, cx));
            }
        });
    }
}

#[cfg(test)]
mod tests;
