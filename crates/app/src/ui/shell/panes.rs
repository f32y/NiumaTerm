use crate::ui::shell::*;

const PANE_RESIZE_STEP: Pixels = px(30.0);

impl Shell {
    pub(super) fn on_split_up(&mut self, _: &SplitUp, window: &mut Window, cx: &mut Context<Self>) {
        self.split_pane(SplitDirection::Up, window, cx);
    }

    pub(super) fn on_split_down(
        &mut self,
        _: &SplitDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_pane(SplitDirection::Down, window, cx);
    }

    pub(super) fn on_split_left(
        &mut self,
        _: &SplitLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_pane(SplitDirection::Left, window, cx);
    }

    pub(super) fn on_split_right(
        &mut self,
        _: &SplitRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_pane(SplitDirection::Right, window, cx);
    }

    /// Create a new pane on the given side of the focused pane. The new shell
    /// starts in the focused pane's live cwd (OSC 7 when reported, launch cwd
    /// otherwise) and becomes the focused pane. A no-op when the focused pane
    /// cannot yield the minimum panel size to the new sibling.
    fn split_pane(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Agent tabs have no pane tree to split.
        let Some(focused) = self.try_active_pane() else {
            return;
        };

        // Guard before mutating: the tree insert and the resizable-state
        // insert must both happen or neither (they are index-aligned).
        let has_room = focused.read(cx).content_size().is_none_or(|size| {
            let extent = match direction.axis() {
                Axis::Horizontal => size.width,
                Axis::Vertical => size.height,
            };
            px(extent.as_f32() / 2.0) >= PANEL_MIN_SIZE
        });

        if !has_room {
            return;
        }

        let cwd = focused.read(cx).tab_state().cwd;
        let id = Self::alloc_id(&mut self.next_id);
        let default_profile = Self::default_profile(cx);

        let pane = Self::spawn_default_pane(cx, id, default_profile, cwd);

        self.register_agent_pane(&pane, cx);

        let tree = self.workspaces.active_tabs_mut().active_mut().live_mut();

        match tree.split(PaneId(id), pane, direction, || {
            cx.new(|_| ResizableState::default())
        }) {
            SplitOutcome::Inserted {
                state,
                index,
                before,
            } => {
                // Halve the focused panel into the new sibling; siblings keep
                // their sizes.
                state.update(cx, |state, cx| state.split_panel(index, before, cx));
            }
            // A fresh two-child split lays out 50/50 on its own.
            SplitOutcome::Wrapped => {}
        }

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(super) fn on_resize_pane_up(
        &mut self,
        _: &ResizePaneUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Up, window, cx);
    }

    pub(super) fn on_resize_pane_down(
        &mut self,
        _: &ResizePaneDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Down, window, cx);
    }

    pub(super) fn on_resize_pane_left(
        &mut self,
        _: &ResizePaneLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Left, window, cx);
    }

    pub(super) fn on_resize_pane_right(
        &mut self,
        _: &ResizePaneRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Right, window, cx);
    }

    /// Resize the focused pane one step along the arrow's axis, in the nearest
    /// ancestor split with a matching axis (tmux semantics: the trailing edge
    /// moves, except for the last child whose only movable edge is the leading
    /// one). A no-op when no matching-axis split exists.
    fn resize_pane(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.workspaces.active_tabs().active().tree() else {
            return;
        };

        let Some((state, index, count)) = tree.resize_split(direction.axis()) else {
            return;
        };

        let Some(current) = state.read(cx).sizes().get(index).copied() else {
            return;
        };

        let grow = direction.positive() == (index + 1 < count);

        let target = if grow {
            current + PANE_RESIZE_STEP
        } else {
            current - PANE_RESIZE_STEP
        };

        state.update(cx, |state, cx| {
            state.resize_panel(index, target, window, cx)
        });

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Focus the pane `id` in the active tab (mouse click).
    pub(crate) fn focus_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = self.workspaces.active_tabs_mut().active_mut().tree_mut() else {
            return;
        };

        if tree.focused() == id || !tree.set_focused(id) {
            return;
        }

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Apply saved split ratios once their groups have real bounds (the first
    /// visible frames after a session restore); cleared after applying.
    pub(super) fn apply_pending_ratios(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = self.workspaces.active_tabs_mut().active_mut().tree_mut() else {
            return;
        };

        tree.for_each_split_mut(&mut |state, pending| {
            if let Some(ratios) = pending.take_if(|_| state.read(cx).has_bounds()) {
                state.update(cx, |state, cx| state.set_ratios(&ratios, cx));
            }
        });
    }

    /// The active tab's pane tree as nested resizable groups. The main surface
    /// owns the outer frame, so a single pane renders without another card.
    pub(super) fn render_active_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(agent) = self.active_agent() {
            return div()
                .size_full()
                .overflow_hidden()
                .child(agent)
                .into_any_element();
        }

        let tree = self.workspaces.active_tabs().active().live();

        let multi = !tree.is_single_leaf();

        Self::render_pane_node(tree.root(), tree.focused(), multi, cx)
    }

    fn render_pane_node(
        node: &PaneNode<Entity<TerminalPane>, Entity<ResizableState>>,
        focused: PaneId,
        multi: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            PaneNode::Leaf { id, pane, .. } => {
                let id = *id;

                div()
                    .size_full()
                    // Split leaves retain equal-width borders so focus changes
                    // never shift layout; the parent surface clips their outer
                    // edges and provides the single-pane frame.
                    .when(multi, |this| {
                        this.border_1().border_color(if id == focused {
                            cx.theme().primary
                        } else {
                            cx.theme().border
                        })
                    })
                    .capture_any_mouse_down(cx.listener(
                        move |this, _: &MouseDownEvent, window, cx| {
                            this.focus_pane(id, window, cx);
                        },
                    ))
                    .child(pane.clone())
                    .into_any_element()
            }
            PaneNode::Split {
                id,
                axis,
                children,
                state,
                ..
            } => {
                let shell = cx.entity();

                let mut group = ResizablePanelGroup::new(("pane-split", *id as usize))
                    .axis(*axis)
                    .with_state(state)
                    // Keep the in-memory session mirror's split ratios fresh
                    // after divider drags (the quit hook reads it).
                    .on_resize(move |_, _, cx| {
                        shell.update(cx, |this, cx| this.sync_session_memory(cx));
                    });

                for child in children {
                    group = group.child(
                        resizable_panel().child(Self::render_pane_node(child, focused, multi, cx)),
                    );
                }
                group.into_any_element()
            }
        }
    }
}
