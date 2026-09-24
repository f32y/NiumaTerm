#[cfg(test)]
#[path = "side_questions_tests.rs"]
mod tests;

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Div, DragMoveEvent, Empty, Entity, EntityId, FontWeight,
    MouseButton, MouseDownEvent, Pixels, Point, Size, Stateful, Window, div, point, px, relative,
    size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, ElementExt as _, IconName, Sizable as _, h_flex, v_flex};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::execution::SessionOwner;
use crate::agent_tab::settings::UI_RADIUS;
use crate::agent_tab::transcript::TranscriptView;

/// Gap kept between the card and the pane edge it starts against.
const CARD_INSET: Pixels = px(12.);

/// The size the window opens at. The transcript inside is a bottom-aligned
/// virtual list, which needs a definite height to lay out against, so the
/// window always has one.
const DEFAULT_SIZE: Size<Pixels> = size(px(420.), px(440.));

/// The size a side thread's window opens at. It carries a whole composer
/// with its settings row under the transcript, so it needs more room than
/// a list of answers.
const THREAD_DEFAULT_SIZE: Size<Pixels> = size(px(480.), px(600.));

/// Smallest size a resize leaves: room for the title bar's buttons and a few
/// transcript rows below it.
const MIN_SIZE: Size<Pixels> = size(px(280.), px(180.));

/// How far a resize handle reaches to either side of the edge it sits on.
/// Straddling the edge keeps the grab area usable without eating into the
/// transcript's scrollbar.
const HANDLE_REACH: Pixels = px(3.);

/// Side length of a corner handle, which resizes both edges it joins.
const CORNER: Pixels = px(12.);

/// The payload a card drag carries, naming the pane that owns the card. Every
/// pane in a split view registers a move listener for this type, so without
/// the owner a drag in one pane would move the card in the other too.
#[derive(Clone)]
pub(crate) struct SideCardDrag(EntityId);

impl Render for SideCardDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Which edges a resize handle moves. A corner moves two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Edges {
    pub(crate) left: bool,
    pub(crate) right: bool,
    pub(crate) top: bool,
    pub(crate) bottom: bool,
}

/// What a press on the card starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gesture {
    Move,
    Resize(Edges),
}

/// A side chat that runs as its own session: a fork of the pane's thread,
/// and the pane presenting it with its own composer and settings.
pub(crate) struct SideThread {
    /// Dropping the owner closes the side session.
    pub(crate) owner: SessionOwner,

    pub(crate) pane: Entity<AgentPane>,

    /// The thread the fork was taken from. A conversation that moves to
    /// another thread leaves this side chat describing one that is gone.
    pub(crate) parent_thread: String,
}

/// The pointer and the card's pane-local bounds when a gesture began.
struct Press {
    pointer: Point<Pixels>,
    start: Bounds<Pixels>,
    gesture: Gesture,
}

/// The floating Side Chat window of one pane: the view rendering its
/// exchanges, whether it is minimized, and where it sits.
///
/// Bounds are pane-local. Pointer events arrive in window coordinates, so a
/// gesture applies the pointer's travel since the press to the bounds the card
/// had then; the pane's own origin never enters the sum.
pub(crate) struct SideChatWindow {
    /// Renders the side conversation through the same view as the pane's own
    /// conversation, so the two cannot drift apart in presentation.
    pub(crate) transcript: Entity<TranscriptView>,

    /// A side thread, where the harness forks one instead of answering side
    /// questions in place. It is presented in place of `transcript`.
    pub(crate) thread: Option<SideThread>,

    /// Minimizing only hides the window; its exchanges and any answer still
    /// on its way are kept for when it is restored.
    pub(crate) minimized: bool,

    /// `None` until the first move or resize, leaving the card at its default
    /// size in its starting corner.
    bounds: Option<Bounds<Pixels>>,

    press: Option<Press>,

    /// The pane's bounds from its last prepaint, in window coordinates. They
    /// keep the card inside the pane when it is dragged or the pane shrinks.
    pane: Rc<Cell<Bounds<Pixels>>>,

    /// The card's bounds from its last prepaint, in window coordinates, which
    /// is where a gesture on a card still in its starting corner begins.
    card: Rc<Cell<Bounds<Pixels>>>,
}

impl SideChatWindow {
    pub(crate) fn new(transcript: Entity<TranscriptView>) -> Self {
        Self {
            transcript,
            thread: None,
            minimized: false,
            bounds: None,
            press: None,
            pane: Rc::default(),
            card: Rc::default(),
        }
    }

    /// Record the pane's bounds; attached to the pane's root element.
    pub(crate) fn track_pane(&self) -> impl Fn(Bounds<Pixels>, &mut Window, &mut App) + 'static {
        let pane = self.pane.clone();

        move |bounds, _, _| pane.set(bounds)
    }

    fn press(&mut self, pointer: Point<Pixels>, gesture: Gesture) {
        let card = self.card.get();

        self.press = Some(Press {
            pointer,
            start: Bounds::new(card.origin - self.pane.get().origin, card.size),
            gesture,
        });
    }

    /// Apply the pointer's travel to the gesture in progress. Returns whether
    /// the card changed.
    fn drag_to(&mut self, pointer: Point<Pixels>) -> bool {
        let Some(press) = &self.press else {
            return false;
        };

        let bounds = dragged(
            press.start,
            press.gesture,
            pointer - press.pointer,
            self.pane.get().size,
        );

        if self.bounds == Some(bounds) {
            return false;
        }

        self.bounds = Some(bounds);

        true
    }
}

/// Where a gesture that began at `start` leaves the card after the pointer
/// travelled `delta`, inside a pane of `pane` size. Every resized edge stops
/// at the pane's edge and at the minimum size, measured against the edge
/// opposite it, so a handle dragged past either limit pins the card rather
/// than flipping or pushing it.
pub(crate) fn dragged(
    start: Bounds<Pixels>,
    gesture: Gesture,
    delta: Point<Pixels>,
    pane: Size<Pixels>,
) -> Bounds<Pixels> {
    let edges = match gesture {
        Gesture::Move => return fit(Bounds::new(start.origin + delta, start.size), pane),
        Gesture::Resize(edges) => edges,
    };

    let mut left = start.left();
    let mut right = start.right();
    let mut top = start.top();
    let mut bottom = start.bottom();

    if edges.left {
        left = (left + delta.x).min(right - MIN_SIZE.width).max(px(0.));
    }

    if edges.right {
        right = (right + delta.x).max(left + MIN_SIZE.width).min(pane.width);
    }

    if edges.top {
        top = (top + delta.y).min(bottom - MIN_SIZE.height).max(px(0.));
    }

    if edges.bottom {
        bottom = (bottom + delta.y)
            .max(top + MIN_SIZE.height)
            .min(pane.height);
    }

    Bounds::from_corners(point(left, top), point(right, bottom))
}

/// `bounds` shrunk to fit a pane of `pane` size and moved fully inside it. A
/// pane smaller than the card pins it to the top-left corner, where its
/// window buttons stay reachable. A pane not measured yet leaves it alone.
pub(crate) fn fit(bounds: Bounds<Pixels>, pane: Size<Pixels>) -> Bounds<Pixels> {
    if pane.width <= px(0.) || pane.height <= px(0.) {
        return bounds;
    }

    let size = size(
        bounds.size.width.min(pane.width),
        bounds.size.height.min(pane.height),
    );

    let origin = point(
        bounds.origin.x.min(pane.width - size.width).max(px(0.)),
        bounds.origin.y.min(pane.height - size.height).max(px(0.)),
    );

    Bounds::new(origin, size)
}

/// One resize handle: a strip along an edge or a square on a corner, which
/// starts a resize of `edges` when pressed and dragged.
fn resize_handle(id: &'static str, edges: Edges, cx: &mut Context<AgentPane>) -> Stateful<Div> {
    let owner = cx.entity_id();

    div()
        .id(id)
        .absolute()
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, _| {
                this.side_chat.press(event.position, Gesture::Resize(edges))
            }),
        )
        .on_drag(SideCardDrag(owner), |drag, _, _, cx| {
            cx.stop_propagation();

            cx.new(|_| drag.clone())
        })
}

fn resize_handles(cx: &mut Context<AgentPane>) -> Vec<Stateful<Div>> {
    let edge = |left, right, top, bottom| Edges {
        left,
        right,
        top,
        bottom,
    };

    vec![
        resize_handle("side-chat-resize-left", edge(true, false, false, false), cx)
            .top(CORNER)
            .bottom(CORNER)
            .left(-HANDLE_REACH)
            .w(HANDLE_REACH * 2.)
            .cursor_ew_resize(),
        resize_handle(
            "side-chat-resize-right",
            edge(false, true, false, false),
            cx,
        )
        .top(CORNER)
        .bottom(CORNER)
        .right(-HANDLE_REACH)
        .w(HANDLE_REACH * 2.)
        .cursor_ew_resize(),
        resize_handle("side-chat-resize-top", edge(false, false, true, false), cx)
            .left(CORNER)
            .right(CORNER)
            .top(-HANDLE_REACH)
            .h(HANDLE_REACH * 2.)
            .cursor_ns_resize(),
        resize_handle(
            "side-chat-resize-bottom",
            edge(false, false, false, true),
            cx,
        )
        .left(CORNER)
        .right(CORNER)
        .bottom(-HANDLE_REACH)
        .h(HANDLE_REACH * 2.)
        .cursor_ns_resize(),
        resize_handle(
            "side-chat-resize-top-left",
            edge(true, false, true, false),
            cx,
        )
        .left(-HANDLE_REACH)
        .top(-HANDLE_REACH)
        .size(CORNER)
        .cursor_nwse_resize(),
        resize_handle(
            "side-chat-resize-bottom-right",
            edge(false, true, false, true),
            cx,
        )
        .right(-HANDLE_REACH)
        .bottom(-HANDLE_REACH)
        .size(CORNER)
        .cursor_nwse_resize(),
        resize_handle(
            "side-chat-resize-top-right",
            edge(false, true, true, false),
            cx,
        )
        .right(-HANDLE_REACH)
        .top(-HANDLE_REACH)
        .size(CORNER)
        .cursor_nesw_resize(),
        resize_handle(
            "side-chat-resize-bottom-left",
            edge(true, false, false, true),
            cx,
        )
        .left(-HANDLE_REACH)
        .bottom(-HANDLE_REACH)
        .size(CORNER)
        .cursor_nesw_resize(),
    ]
}

/// The side chat in a window floating over the pane. It is separate from the
/// transcript because none of this is part of the conversation. Its title bar
/// moves it and its edges resize it, so it can be fitted around whatever it
/// would otherwise cover.
pub(crate) fn side_chat_window(window: &SideChatWindow, cx: &mut Context<AgentPane>) -> AnyElement {
    let owner = cx.entity_id();
    let card_bounds = window.card.clone();

    let card = match window.bounds {
        Some(bounds) => {
            let bounds = fit(bounds, window.pane.get().size);

            div()
                .left(bounds.origin.x)
                .top(bounds.origin.y)
                .w(bounds.size.width)
                .h(bounds.size.height)
        }
        None => {
            let size = match window.thread {
                Some(_) => THREAD_DEFAULT_SIZE,
                None => DEFAULT_SIZE,
            };

            div()
                .top(CARD_INSET)
                .right(CARD_INSET)
                .w(size.width)
                .h(size.height)
                .max_w(relative(1.))
                .max_h(relative(1.))
        }
    };

    // Only answers given in place need telling how to follow up; a side
    // thread's own composer says that by being there. The slot fills the row
    // either way, keeping the window buttons at its far end.
    let hint = div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .children(window.thread.is_none().then(|| t!("agent-side-hint")));

    let content = match &window.thread {
        // A side thread's pane brings its own transcript, composer, and
        // settings row, and handles its own selections.
        Some(thread) => div().flex_1().min_h_0().child(thread.pane.clone()),
        None => div()
            .flex_1()
            .min_h_0()
            // Releasing a selection offers the same copy and quote menu
            // the pane's own transcript does, which is how an answer
            // reaches the composer.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(AgentPane::on_transcript_mouse_up),
            )
            .child(window.transcript.clone()),
    };

    card.absolute()
        .occlude()
        .on_prepaint(move |bounds, _, _| card_bounds.set(bounds))
        // Every gesture reports its moves here. GPUI calls drag-move listeners
        // on every pointer move of an active drag, wherever the pointer is, so
        // one listener serves the title bar and every handle, and fast drags
        // that outrun the handle still land. Changing the card is a pointer
        // event outside any frame, so it has to wake the frame pump itself.
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<SideCardDrag>, _, cx| {
                if event.drag(cx).0 == owner && this.side_chat.drag_to(event.event.position) {
                    cx.notify();
                }
            }),
        )
        .child(
            v_flex()
                .size_full()
                .rounded(UI_RADIUS)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .shadow_lg()
                .overflow_hidden()
                .child(
                    h_flex()
                        .id("side-chat-title")
                        .w_full()
                        .flex_none()
                        .px_3()
                        .py_0p5()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .cursor_grab()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, _| {
                                this.side_chat.press(event.position, Gesture::Move)
                            }),
                        )
                        .on_drag(SideCardDrag(owner), |drag, _, _, cx| {
                            cx.stop_propagation();

                            cx.new(|_| drag.clone())
                        })
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("agent-side-title")),
                        )
                        .child(hint)
                        .child(
                            Button::new("side-chat-minimize")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Minus)
                                .tooltip(t!("agent-side-minimize"))
                                .accessibility_label(t!("agent-side-minimize"))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_side_chat(cx))),
                        )
                        .child(
                            Button::new("side-chat-close")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip(t!("agent-side-close"))
                                .accessibility_label(t!("agent-side-close"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_close_side_chat(window, cx)
                                })),
                        ),
                )
                .child(content),
        )
        // Painted after the content so the handles win the edge over it.
        .children(resize_handles(cx))
        .into_any_element()
}
