use gpui::{
    Context, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, Window,
};
use nmt_terminal::input::WheelDelta;
use nmt_terminal::session::SurfaceMouseButton;

use crate::pane_model::mouse::{MouseInput, MouseOutcome};
use crate::pane_model::viewport::LocalPoint;
use crate::view::TerminalPane;
use crate::view::key::modifiers_state;

impl TerminalPane {
    pub(crate) fn local_position(&self, position: Point<Pixels>) -> LocalPoint {
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
        }
    }

    fn apply_mouse_outcome(&mut self, outcome: MouseOutcome, cx: &mut Context<Self>) {
        match outcome {
            MouseOutcome::Ignored => {}
            MouseOutcome::OpenUrl(url) => cx.open_url(&url),
            MouseOutcome::SelectionChanged | MouseOutcome::HoverChanged => cx.notify(),
            MouseOutcome::FrozenSelectionStarted => {
                self.invalidate(cx);
                cx.notify();
            }
            MouseOutcome::EngineHandled => self.invalidate(cx),
            MouseOutcome::Scrolled(outcome) => {
                self.apply_scroll_outcome(outcome, cx);
            }
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
        let input = self.mouse_input(event.position, Some(event.button), event.modifiers, 1);
        let release = self.model.mouse_up(input);
        if release.scrollbar_released {
            self.mark_scrollbar_activity(cx);
        }
        self.apply_mouse_outcome(release.outcome, cx);
    }

    pub(crate) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cell_metrics(window, cx);
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
        let cell = self.cell_metrics(window, cx);
        let delta = match event.delta {
            ScrollDelta::Lines(point) => WheelDelta::Steps(point.y),
            ScrollDelta::Pixels(point) => {
                WheelDelta::Rows(point.y.as_f32() / cell.height_px.max(1.0))
            }
        };
        let outcome = self.model.scroll_wheel(
            self.local_position(event.position),
            delta,
            modifiers_state(event.modifiers),
        );
        if outcome.hover_changed {
            cx.notify();
        }
        if outcome.handled {
            self.mark_scrollbar_activity(cx);
            self.invalidate(cx);
        }
    }
}
