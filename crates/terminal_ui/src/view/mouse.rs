use gpui::{
    Context, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, Window,
};
use nmt_terminal::session::SurfaceMouseButton;

use crate::input::modifiers_state;
use crate::metrics::CellMetrics;
use crate::pane_model::mouse::{MouseInput, MouseOutcome};
use crate::pane_model::viewport::LocalPoint;
use crate::view::TerminalPane;
use crate::view::links::follows_link;

pub(super) fn terminal_scroll_lines(delta: ScrollDelta, cell: CellMetrics) -> i32 {
    let raw = match delta {
        ScrollDelta::Lines(point) => point.y * 3.0,
        ScrollDelta::Pixels(point) => point.y.as_f32() / cell.height_px.max(1.0),
    };
    if raw.abs() < 0.5 {
        0
    } else {
        raw.round() as i32
    }
}

impl TerminalPane {
    pub(super) fn local_position(&self, position: Point<Pixels>) -> LocalPoint {
        let origin = self.content_origin();
        LocalPoint {
            x: (position.x - origin.x).as_f32(),
            y: (position.y - origin.y).as_f32(),
        }
    }

    fn mouse_input(
        &self,
        position: Point<Pixels>,
        button: Option<MouseButton>,
        modifiers: Modifiers,
        click_count: usize,
    ) -> MouseInput {
        MouseInput {
            position: self.local_position(position),
            button: button.and_then(|button| match button {
                MouseButton::Left => Some(SurfaceMouseButton::Left),
                MouseButton::Middle => Some(SurfaceMouseButton::Middle),
                MouseButton::Right => Some(SurfaceMouseButton::Right),
                MouseButton::Navigate(_) => None,
            }),
            modifiers: modifiers_state(modifiers),
            click_count,
            follow_link: follows_link(modifiers),
        }
    }

    fn apply_mouse_outcome(&mut self, outcome: MouseOutcome, cx: &mut Context<Self>) {
        match outcome {
            MouseOutcome::Ignored => {}
            MouseOutcome::OpenUrl(url) => cx.open_url(&url),
            MouseOutcome::SelectionChanged => cx.notify(),
            MouseOutcome::FrozenSelectionStarted => {
                self.invalidate(cx);
                cx.notify();
            }
            MouseOutcome::EngineHandled => self.invalidate(cx),
        }
    }

    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.cell_metrics(window, cx);
        let input = self.mouse_input(
            event.position,
            Some(event.button),
            event.modifiers,
            event.click_count,
        );
        let outcome = self.model.mouse_down(input);
        self.apply_mouse_outcome(outcome, cx);
    }

    pub(super) fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cell_metrics(window, cx);
        if self.model.scrollbar.is_dragging() {
            self.mark_scrollbar_activity(cx);
        }
        self.model.scrollbar.end_drag();
        let input = self.mouse_input(event.position, Some(event.button), event.modifiers, 1);
        let outcome = self.model.mouse_up(input);
        self.apply_mouse_outcome(outcome, cx);
    }

    pub(crate) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cell_metrics(window, cx);
        let local = self.local_position(event.position);
        self.model.links.record_position(local);
        if event.pressed_button.is_none() {
            self.update_hovered_link(event.position, event.modifiers, cx);
        }
        if self.model.scrollbar.is_dragging() {
            self.scroll_thumb_to(
                self.model
                    .scrollbar
                    .thumb_top_for(self.scrollbar_fraction(event.position.y)),
                cx,
            );
            return;
        }
        let input = self.mouse_input(event.position, event.pressed_button, event.modifiers, 1);
        let outcome = self.model.mouse_move(input);
        self.apply_mouse_outcome(outcome, cx);
    }

    pub(super) fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model.links.clear() {
            cx.notify();
        }
        let cell = self.cell_metrics(window, cx);
        let lines = terminal_scroll_lines(event.delta, cell);
        if self.model.scroll_wheel(
            self.local_position(event.position),
            lines,
            modifiers_state(event.modifiers),
        ) {
            self.mark_scrollbar_activity(cx);
            self.invalidate(cx);
        }
    }
}
