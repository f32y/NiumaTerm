//! The orchestration canvas: node cards placed by a viewport transform, and
//! one painted layer of dependency edges drawn as cubic curves.
//!
//! Cards are plain GPUI elements so text layout and clicks work as anywhere
//! else; only the edges are custom-painted. Pointer events arrive outside a
//! frame, so every handler that changes what is shown calls `cx.notify`.
//!
//! An editable canvas reports what the user did as events and leaves the
//! definition to its owner, which answers with the next `show`. A run's
//! canvas only pans, zooms and opens node details.

mod geometry;
mod gesture;
mod viewport;

#[cfg(test)]
#[path = "geometry_tests.rs"]
mod geometry_tests;
#[cfg(test)]
#[path = "gesture_tests.rs"]
mod gesture_tests;
#[cfg(test)]
#[path = "viewport_tests.rs"]
mod viewport_tests;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, Render,
    ScrollDelta, ScrollWheelEvent, Window, canvas, div, px, size,
};
use gpui_component::ActiveTheme as _;
use nmt_agent::orchestration::definition::{Definition, Position};
use rust_i18n::t;

use crate::agent_tab::orchestration::canvas::geometry::{
    CARD_HEIGHT, CARD_WIDTH, PORT_RADIUS, edge_curve, hit_card, hit_edge, hit_port, input_point,
    port_center,
};
use crate::agent_tab::orchestration::canvas::gesture::{Gesture, Moved, Released, Target};
use crate::agent_tab::orchestration::canvas::viewport::Viewport;
use crate::agent_tab::settings::UI_RADIUS;

/// Below this zoom a card shows only its id; its state line would be too
/// small to read.
const DETAIL_ZOOM: f32 = 0.5;

/// Zoom change per scrolled line with Ctrl held.
const ZOOM_STEP: f32 = 1.1;

/// Pixels of a precise scroll that count as one line of zoom.
const PIXELS_PER_ZOOM_STEP: f32 = 50.;

/// A node's run state, as the card shows it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CardState {
    pub(super) label: String,
    pub(super) color: Hsla,
    pub(super) needs_input: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CanvasNode {
    pub(super) id: String,
    pub(super) position: Position,

    /// Present on a run's canvas, which also offers each node's details.
    pub(super) state: Option<CardState>,

    /// A validation error names this node.
    pub(super) marked: bool,
}

/// A selected node or edge, by index into what was last shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Selection {
    Node(usize),
    Edge(usize),
}

pub(super) enum CanvasEvent {
    OpenDetails(usize),
    Selected,
    /// A card was dropped with its top-left corner at `to`.
    Moved {
        node: usize,
        to: Position,
    },
    /// A line from `from`'s output port was dropped on `to`.
    Connect {
        from: usize,
        to: usize,
    },
    Delete(Selection),
}

pub(super) struct GraphCanvas {
    #[cfg(test)]
    pub(super) render_count: usize,

    /// Which graph is shown; a new one is fitted into view once laid out.
    key: String,

    nodes: Vec<CanvasNode>,

    /// Dependency edges as (dependency, dependent) node indices.
    edges: Vec<(usize, usize)>,

    editable: bool,
    selection: Option<Selection>,
    viewport: Viewport,
    gesture: Gesture,
    focus: FocusHandle,

    /// The element's bounds from its last layout, for fitting and for
    /// turning window positions into canvas-relative ones.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,

    fit_pending: bool,
}

impl EventEmitter<CanvasEvent> for GraphCanvas {}

impl Focusable for GraphCanvas {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GraphCanvas {
    pub(super) fn new(editable: bool, cx: &mut Context<Self>) -> Self {
        Self {
            #[cfg(test)]
            render_count: 0,
            key: String::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            editable,
            selection: None,
            viewport: Viewport::default(),
            gesture: Gesture::Idle,
            focus: cx.focus_handle(),
            bounds: Rc::new(Cell::new(None)),
            fit_pending: true,
        }
    }

    /// Show `nodes` and `edges` as the graph named `key`. Showing a
    /// different graph fits it into view; refreshing the same one keeps the
    /// user's pan and zoom. A selection that no longer exists is dropped.
    pub(super) fn show(
        &mut self,
        key: &str,
        nodes: Vec<CanvasNode>,
        edges: Vec<(usize, usize)>,
        cx: &mut Context<Self>,
    ) {
        let graph_changed = self.key != key;
        let content_changed = self.nodes != nodes || self.edges != edges;

        if graph_changed {
            self.key = key.to_owned();
            self.fit_pending = true;
            self.selection = None;
        }

        if content_changed {
            self.nodes = nodes;
            self.edges = edges;

            let exists = match self.selection {
                Some(Selection::Node(node)) => node < self.nodes.len(),
                Some(Selection::Edge(edge)) => edge < self.edges.len(),
                None => true,
            };

            if !exists {
                self.selection = None;
            }
        }

        // A different graph resets selection and fitting even when its
        // nodes and edges match the previous graph.
        if graph_changed || content_changed {
            cx.notify();
        }
    }

    pub(super) fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub(super) fn select(&mut self, selection: Option<Selection>, cx: &mut Context<Self>) {
        if self.selection != selection {
            self.selection = selection;

            cx.notify();
        }
    }

    /// Fit every node into view on the next render.
    pub(super) fn fit(&mut self, cx: &mut Context<Self>) {
        self.fit_pending = true;

        cx.notify();
    }

    /// Where a new card goes: centered in the visible area.
    pub(super) fn visible_center(&self) -> Position {
        let size = self
            .bounds
            .get()
            .map_or(size(px(0.), px(0.)), |bounds| bounds.size);

        let center = self
            .viewport
            .to_canvas(Point::new(size.width / 2., size.height / 2.));

        Position {
            x: center.x - CARD_WIDTH / 2.,
            y: center.y - CARD_HEIGHT / 2.,
        }
    }

    /// Each node's position as drawn, following the card being dragged.
    fn shown_positions(&self) -> Vec<Position> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| match self.gesture.drag_offset(index) {
                Some(offset) => Position {
                    x: node.position.x + f32::from(offset.x) / self.viewport.zoom,
                    y: node.position.y + f32::from(offset.y) / self.viewport.zoom,
                },
                None => node.position,
            })
            .collect()
    }

    fn local(&self, window_position: Point<Pixels>) -> Point<Pixels> {
        let origin = self
            .bounds
            .get()
            .map_or(Point::default(), |bounds| bounds.origin);

        window_position - origin
    }

    fn on_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.local(event.position);
        let positions = self.shown_positions();

        let target = if self.editable
            && let Some(node) = hit_port(at, &positions, self.viewport)
        {
            Target::Port(node)
        } else if let Some(node) = hit_card(at, &positions, self.viewport) {
            Target::Card(node)
        } else {
            Target::Background
        };

        self.gesture = Gesture::press(target, at, self.editable);

        self.focus.focus(window, cx);

        cx.notify();
    }

    fn on_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let held = event.pressed_button == Some(MouseButton::Left);

        match self.gesture.moved(self.local(event.position), held) {
            Moved::Nothing => {}
            Moved::Pan(delta) => {
                self.viewport.pan(delta);

                cx.notify();
            }
            Moved::Redraw => cx.notify(),
        }
    }

    fn on_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let at = self.local(event.position);
        let positions = self.shown_positions();
        let over = hit_card(at, &positions, self.viewport);

        match self.gesture.release(at, over) {
            Released::Nothing => {}
            Released::BackgroundClick(at) if self.editable => {
                self.selection =
                    hit_edge(at, &self.edges, &positions, self.viewport).map(Selection::Edge);

                cx.emit(CanvasEvent::Selected);
            }
            Released::BackgroundClick(_) => {}
            Released::CardClick(node) => {
                self.selection = Some(Selection::Node(node));

                cx.emit(CanvasEvent::Selected);
            }
            Released::Dragged { node, by } => {
                let from = self.nodes[node].position;

                self.selection = Some(Selection::Node(node));

                cx.emit(CanvasEvent::Selected);

                cx.emit(CanvasEvent::Moved {
                    node,
                    to: Position {
                        x: from.x + f32::from(by.x) / self.viewport.zoom,
                        y: from.y + f32::from(by.y) / self.viewport.zoom,
                    },
                });
            }
            Released::Connected { from, to } => cx.emit(CanvasEvent::Connect { from, to }),
        }

        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &Window, cx: &mut Context<Self>) {
        if event.modifiers.control {
            let lines = match event.delta {
                ScrollDelta::Lines(delta) => delta.y,
                ScrollDelta::Pixels(delta) => f32::from(delta.y) / PIXELS_PER_ZOOM_STEP,
            };

            self.viewport
                .zoom_at(self.local(event.position), ZOOM_STEP.powf(lines));
        } else {
            self.viewport
                .pan(event.delta.pixel_delta(window.line_height()));
        }

        cx.stop_propagation();

        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if !self.editable || !matches!(event.keystroke.key.as_str(), "delete" | "backspace") {
            return;
        }

        if let Some(selection) = self.selection.take() {
            cx.emit(CanvasEvent::Delete(selection));

            cx.stop_propagation();

            cx.notify();
        }
    }

    fn card(&self, index: usize, position: Position, cx: &mut Context<Self>) -> impl IntoElement {
        let node = &self.nodes[index];
        let theme = cx.theme();
        let zoom = self.viewport.zoom;
        let origin = self.viewport.to_screen(position);
        let detailed = zoom >= DETAIL_ZOOM;
        let selected = self.selection == Some(Selection::Node(index));

        let needs_input = node.state.as_ref().is_some_and(|state| state.needs_input);

        let border = if selected {
            theme.primary
        } else if node.marked {
            theme.danger
        } else if needs_input {
            theme.warning
        } else {
            theme.border
        };

        div()
            .id(("orchestration-node", index))
            .debug_selector(move || format!("orchestration-node-{index}"))
            .absolute()
            .left(origin.x)
            .top(origin.y)
            .w(px(CARD_WIDTH * zoom))
            .h(px(CARD_HEIGHT * zoom))
            .p(px(8. * zoom))
            .flex()
            .flex_col()
            .gap(px(4. * zoom))
            .overflow_hidden()
            .rounded(UI_RADIUS)
            .border_1()
            .when(selected, |card| card.border_2())
            .border_color(border)
            .bg(theme.background)
            .child(
                div()
                    .text_size(px(14. * zoom))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(node.id.clone()),
            )
            .when_some(node.state.clone().filter(|_| detailed), |card, state| {
                card.child(
                    div()
                        .text_size(px(12. * zoom))
                        .text_color(state.color)
                        .truncate()
                        .child(state.label),
                )
                .when(state.needs_input, |card| {
                    card.child(
                        div()
                            .text_size(px(12. * zoom))
                            .text_color(theme.warning)
                            .truncate()
                            .child(t!("orchestration-node-needs-input").into_owned()),
                    )
                })
                .child(
                    div()
                        .id(("orchestration-node-details", index))
                        .text_size(px(12. * zoom))
                        .text_color(theme.link)
                        .cursor_pointer()
                        .child(t!("orchestration-view-details").into_owned())
                        .on_click(
                            cx.listener(move |_, _, _, cx| {
                                cx.emit(CanvasEvent::OpenDetails(index))
                            }),
                        ),
                )
            })
    }

    /// The handle on a card's right edge that a connection is dragged from.
    fn port(&self, position: Position, cx: &App) -> impl IntoElement {
        let center = port_center(position, self.viewport);

        div()
            .absolute()
            .left(center.x - px(PORT_RADIUS))
            .top(center.y - px(PORT_RADIUS))
            .size(px(PORT_RADIUS * 2.))
            .rounded_full()
            .border_1()
            .border_color(cx.theme().muted_foreground)
            .bg(cx.theme().background)
            .cursor_crosshair()
    }
}

impl Render for GraphCanvas {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count += 1;
        }

        // Fitting needs the element's size, known once it has been laid out;
        // the paint below asks for this render again when it was not.
        if self.fit_pending
            && let Some(bounds) = self.bounds.get()
        {
            let positions: Vec<Position> = self.nodes.iter().map(|node| node.position).collect();

            self.viewport = Viewport::fit(&positions, size(CARD_WIDTH, CARD_HEIGHT), bounds.size);
            self.fit_pending = false;
        }

        let viewport = self.viewport;
        let positions = self.shown_positions();
        let theme = cx.theme();
        let edge_color = theme.muted_foreground;
        let selected_color = theme.primary;

        // Each edge leaves the dependency's port and enters the dependent's
        // left edge at mid height, in canvas-relative points.
        let mut curves: Vec<([Point<Pixels>; 4], Hsla)> = self
            .edges
            .iter()
            .enumerate()
            .filter_map(|(index, &(from, to))| {
                let start = port_center(*positions.get(from)?, viewport);
                let end = input_point(*positions.get(to)?, viewport);

                let color = if self.selection == Some(Selection::Edge(index)) {
                    selected_color
                } else {
                    edge_color
                };

                Some((edge_curve(start, end, viewport.zoom), color))
            })
            .collect();

        if let Gesture::Connecting { from, now } = self.gesture
            && let Some(position) = positions.get(from)
        {
            curves.push((
                edge_curve(port_center(*position, viewport), now, viewport.zoom),
                selected_color,
            ));
        }

        let bounds = self.bounds.clone();
        let fit_pending = self.fit_pending;
        let canvas_entity = cx.entity_id();

        let layer = canvas(
            move |layout, _, _| bounds.set(Some(layout)),
            move |layout, _, window, cx| {
                for ([start, first, second, end], color) in curves {
                    let mut path = PathBuilder::stroke(px(1.5));

                    path.move_to(layout.origin + start);

                    path.cubic_bezier_to(
                        layout.origin + end,
                        layout.origin + first,
                        layout.origin + second,
                    );

                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
                    }
                }

                // This layout is what a pending fit measures, so the canvas
                // renders again to apply it. A view marked dirty during paint
                // is cleared when the frame finishes, so the window asks for
                // it again from a next-frame callback, which a frame queued
                // during paint always runs. The direct mark serves a test
                // that draws frames itself and runs no frame callbacks.
                if fit_pending {
                    cx.notify(canvas_entity);

                    window.on_next_frame(move |_, cx| cx.notify(canvas_entity));
                }
            },
        )
        .absolute()
        .size_full();

        let mut cards = Vec::with_capacity(self.nodes.len() * 2);

        for (index, position) in positions.iter().enumerate() {
            cards.push(self.card(index, *position, cx).into_any_element());
        }

        // Ports come after every card so a neighbouring card never covers
        // one, matching the hit test that checks ports first.
        if self.editable {
            for position in &positions {
                cards.push(self.port(*position, cx).into_any_element());
            }
        }

        div()
            .id("orchestration-canvas")
            .debug_selector(|| "orchestration-canvas".into())
            .relative()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.on_down(event, window, cx)
                }),
            )
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| this.on_move(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| this.on_up(event, cx)),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                this.on_scroll(event, window, cx)
            }))
            .child(layer)
            .children(cards)
            .child(
                div()
                    .id("orchestration-canvas-fit")
                    .absolute()
                    .right(px(8.))
                    .bottom(px(8.))
                    .px_2()
                    .py_1()
                    .rounded(UI_RADIUS)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .text_xs()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.fit(cx)))
                    .child(t!("orchestration-canvas-fit").into_owned()),
            )
    }
}

/// Every dependency as (dependency, dependent) node indices, skipping ids
/// that name no node so a definition that does not validate still draws.
pub(super) fn dependency_edges(definition: &Definition) -> Vec<(usize, usize)> {
    let index: HashMap<&str, usize> = definition
        .nodes
        .iter()
        .enumerate()
        .map(|(position, node)| (node.id.as_str(), position))
        .collect();

    definition
        .nodes
        .iter()
        .enumerate()
        .flat_map(|(dependent, node)| {
            node.needs
                .iter()
                .filter_map(|id| {
                    index
                        .get(id.as_str())
                        .map(|&dependency| (dependency, dependent))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}
