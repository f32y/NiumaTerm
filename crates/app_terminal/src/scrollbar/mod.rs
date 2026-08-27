use std::time;

use gpui::prelude::*;
use gpui::{
    Context, Div, DragMoveEvent, Empty, MouseButton, MouseDownEvent, Stateful, div, px, relative,
};
use gpui_component::ActiveTheme;
use gpui_component::scroll::{SCROLLBAR_AUTO_HIDE_DELAY, SCROLLBAR_FADE_OUT_DURATION};
use nmt_terminal::ghostty::ScrollbarInfo;

use super::view::TerminalPane;

struct ScrollbarDrag;

pub(super) fn scrollbar_thumb_geometry(total: f64, offset: f64, len: f64) -> Option<(f32, f32)> {
    if total <= len {
        return None;
    }

    let thumb_height = (len / total).clamp(0.03, 1.0) as f32;
    let scrollable = total - len;
    let thumb_top = (offset.clamp(0.0, scrollable) / scrollable) as f32 * (1.0 - thumb_height);

    Some((thumb_top, thumb_height))
}

pub(super) fn scrollbar_offset_for_thumb(total: f64, len: f64, thumb_top: f32) -> Option<f64> {
    let (_, thumb_height) = scrollbar_thumb_geometry(total, 0.0, len)?;
    let thumb_travel = 1.0 - thumb_height;

    Some((thumb_top / thumb_travel).clamp(0.0, 1.0) as f64 * (total - len))
}

pub(super) fn scrollbar_element(
    sb: ScrollbarInfo,
    opacity: f32,
    cx: &mut Context<TerminalPane>,
) -> Option<Stateful<Div>> {
    let (thumb_top, thumb_height) =
        scrollbar_thumb_geometry(sb.total as f64, sb.offset as f64, sb.len as f64)?;

    Some(
        div()
            .id("terminal-scrollbar")
            .absolute()
            .top_0()
            .right_0()
            .h_full()
            .w(px(10.0))
            .opacity(opacity)
            .hover(|this| this.opacity(1.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.scrollbar_dragging = true;

                    let fraction = this.scrollbar_fraction(event.position.y);

                    if (thumb_top..thumb_top + thumb_height).contains(&fraction) {
                        // Grab the thumb where the pointer hit it — no jump.
                        this.scrollbar_grab = fraction - thumb_top;
                    } else {
                        // Track click: center the thumb on the pointer.
                        this.scrollbar_grab = thumb_height / 2.0;
                        this.scroll_thumb_to(fraction - this.scrollbar_grab, cx);
                    }

                    this.mark_scroll_activity(cx);
                }),
            )
            .on_drag(ScrollbarDrag, |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| Empty)
            })
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ScrollbarDrag>, window, cx| {
                    if this.scrollbar_dragging {
                        cx.stop_propagation();
                        this.on_mouse_move(&event.event, window, cx);
                    }
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(relative(thumb_top))
                    .h(relative(thumb_height))
                    .w_full()
                    .rounded_full()
                    .bg(cx.theme().tokens.scrollbar_thumb),
            ),
    )
}

pub(super) fn scrollbar_opacity(
    dragging: bool,
    elapsed_since_scroll: Option<time::Duration>,
) -> Option<f32> {
    if dragging {
        return Some(1.0);
    }

    let elapsed = elapsed_since_scroll?;

    if elapsed < SCROLLBAR_AUTO_HIDE_DELAY {
        return Some(1.0);
    }

    if elapsed >= SCROLLBAR_AUTO_HIDE_DELAY + SCROLLBAR_FADE_OUT_DURATION {
        return None;
    }

    let fade_elapsed = elapsed - SCROLLBAR_AUTO_HIDE_DELAY;

    Some(1.0 - fade_elapsed.as_secs_f32() / SCROLLBAR_FADE_OUT_DURATION.as_secs_f32())
}

#[cfg(test)]
mod tests;
