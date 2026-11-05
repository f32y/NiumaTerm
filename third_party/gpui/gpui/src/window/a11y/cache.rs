use accesskit::{Action, Node, NodeId};
use collections::FxHashMap;

#[cfg(debug_assertions)]
use crate::window::a11y::debug::NodeDebugInfo;
use crate::window::a11y::{A11y, RetainedA11yActionListener};
use crate::{Bounds, FocusId, Pixels, Point};

pub(crate) struct A11yCacheIndex {
    nodes: usize,
    children: usize,
    focus: Option<NodeId>,
    active_descendant: Option<NodeId>,
}

pub(crate) struct CachedA11y {
    can_translate: bool,
    nodes: Vec<(NodeId, Node)>,
    roots: Vec<NodeId>,
    focus_ids: Vec<(NodeId, FocusId)>,
    bounds: Vec<(NodeId, Bounds<Pixels>)>,
    focus: Option<NodeId>,
    active_descendant: Option<NodeId>,
    actions: FxHashMap<NodeId, Vec<(Action, RetainedA11yActionListener)>>,
    #[cfg(debug_assertions)]
    info: Vec<(NodeId, NodeDebugInfo)>,
}

pub(crate) fn index(a11y: &A11y) -> Option<A11yCacheIndex> {
    a11y.is_active().then(|| A11yCacheIndex {
        nodes: a11y.nodes.all_nodes.len(),
        children: a11y
            .nodes
            .nodes_stack
            .last()
            .map_or(0, |node| node.children().len()),
        focus: a11y.nodes.focus,
        active_descendant: a11y.nodes.active_descendant,
    })
}

pub(crate) fn capture(a11y: &A11y, start: A11yCacheIndex) -> CachedA11y {
    let nodes = a11y.nodes.all_nodes[start.nodes..].to_vec();
    let roots = a11y
        .nodes
        .nodes_stack
        .last()
        .map_or_else(Vec::new, |parent| {
            parent.children()[start.children..].to_vec()
        });
    CachedA11y {
        can_translate: nodes.iter().all(|(id, node)| {
            node.transform().is_none()
                && (node.bounds().is_none() || a11y.node_bounds.contains_key(id))
        }),
        focus_ids: nodes
            .iter()
            .filter_map(|(id, _)| a11y.focus_ids.get(id).map(|focus| (*id, *focus)))
            .collect(),
        bounds: nodes
            .iter()
            .filter_map(|(id, _)| a11y.node_bounds.get(id).map(|bounds| (*id, *bounds)))
            .collect(),
        focus: (a11y.nodes.focus != start.focus)
            .then_some(a11y.nodes.focus)
            .flatten(),
        active_descendant: (a11y.nodes.active_descendant != start.active_descendant)
            .then_some(a11y.nodes.active_descendant)
            .flatten(),
        actions: FxHashMap::default(),
        #[cfg(debug_assertions)]
        info: nodes
            .iter()
            .filter_map(|(id, _)| a11y.nodes.node_info.get(id).map(|info| (*id, info.clone())))
            .collect(),
        nodes,
        roots,
    }
}

impl CachedA11y {
    pub(crate) fn can_translate(&self) -> bool {
        self.can_translate
    }

    pub(crate) fn translate(&mut self, offset: Point<Pixels>, scale: f32) {
        if offset == Point::default() {
            return;
        }
        for (_, bounds) in &mut self.bounds {
            bounds.origin += offset;
        }
        for (_, node) in &mut self.nodes {
            if let Some(mut bounds) = node.bounds() {
                let x = f64::from(offset.x.0 * scale);
                let y = f64::from(offset.y.0 * scale);
                bounds.x0 += x;
                bounds.x1 += x;
                bounds.y0 += y;
                bounds.y1 += y;
                node.set_bounds(bounds);
            }
        }
    }

    pub(crate) fn prepaint(&self, a11y: &mut A11y) {
        for (id, node) in &self.nodes {
            if a11y.nodes.can_push(*id) {
                a11y.nodes.all_nodes.push((*id, node.clone()));
            }
        }
        if let Some(parent) = a11y.nodes.nodes_stack.last_mut() {
            for root in &self.roots {
                parent.push_child(*root);
            }
        }
        a11y.focus_ids.extend(self.focus_ids.iter().copied());
        a11y.node_bounds.extend(self.bounds.iter().copied());
        if let Some(focus) = self.focus {
            a11y.nodes.set_focus(focus);
        }
        if let Some(active_descendant) = self.active_descendant {
            a11y.nodes.set_active_descendant(active_descendant);
        }
        #[cfg(debug_assertions)]
        a11y.nodes.node_info.extend(self.info.iter().cloned());
    }

    pub(crate) fn capture_actions(&mut self, a11y: &A11y) {
        self.actions.clear();
        for (id, _) in &self.nodes {
            if let Some(actions) = a11y.action_listeners.get(id) {
                self.actions.insert(*id, actions.clone());
            }
        }
    }

    pub(crate) fn paint(&self, a11y: &mut A11y) {
        for (id, actions) in &self.actions {
            a11y.action_listeners.insert(*id, actions.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use accesskit::{Node, NodeId, Rect, Role};

    use crate::window::a11y::A11y;
    use crate::window::a11y::cache::{capture, index};
    use crate::{Bounds, point, px, size};

    #[test]
    fn moved_cache_updates_logical_and_physical_node_bounds() {
        let mut a11y = A11y::new(Arc::new(AtomicBool::new(true)), false, None);
        a11y.sync_active_flag();
        let id = NodeId(1);
        a11y.begin_frame();
        let start = index(&a11y).unwrap();
        let mut node = Node::new(Role::Button);
        node.set_bounds(Rect::new(20., 40., 120., 100.));
        assert!(a11y.nodes.push_leaf(id, node));
        a11y.node_bounds.insert(
            id,
            Bounds::new(point(px(10.), px(20.)), size(px(50.), px(30.))),
        );
        let mut cached = capture(&a11y, start);
        assert!(cached.can_translate());
        for (offset, y) in [(10., 30.), (15., 45.), (-5., 40.)] {
            cached.translate(point(px(0.), px(offset)), 2.);
            a11y.begin_frame();
            cached.prepaint(&mut a11y);
            assert_eq!(a11y.node_bounds[&id].top(), px(y));
            let update = a11y.nodes.finalize();
            let node = &update
                .nodes
                .iter()
                .find(|(node_id, _)| *node_id == id)
                .unwrap()
                .1;
            assert_eq!(node.bounds().unwrap().y0, f64::from(y * 2.));
        }
    }

    #[test]
    fn cached_synthetic_children_keep_their_parent_and_focus() {
        let mut a11y = A11y::new(Arc::new(AtomicBool::new(true)), false, None);
        a11y.sync_active_flag();
        let parent = NodeId(1);
        let container = NodeId(2);
        let child = NodeId(3);
        a11y.begin_frame();
        assert!(a11y.nodes.push(parent, Node::new(Role::List)));
        a11y.nodes.set_focus(parent);
        let start = index(&a11y).unwrap();
        assert!(a11y.nodes.push(container, Node::new(Role::ListItem)));
        let mut text = Node::new(Role::Label);
        text.set_value("Cached child");
        text.set_bounds(Rect::new(1., 2., 30., 40.));
        assert!(a11y.nodes.push_leaf(child, text));
        a11y.nodes.set_active_descendant(child);
        a11y.nodes.pop();
        let cached = capture(&a11y, start);
        assert!(!cached.can_translate());
        a11y.nodes.pop();
        let original = a11y.nodes.finalize();

        for _ in 0..3 {
            a11y.begin_frame();
            assert!(a11y.nodes.push(parent, Node::new(Role::List)));
            a11y.nodes.set_focus(parent);
            cached.prepaint(&mut a11y);
            a11y.nodes.pop();
            let reused = a11y.nodes.finalize();
            assert_eq!(reused.focus, child);
            assert_eq!(reused.nodes, original.nodes);
        }
    }
}
