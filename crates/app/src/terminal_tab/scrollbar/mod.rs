use gpui::prelude::*;
use gpui::{
    Context, Div, DragMoveEvent, Empty, MouseButton, MouseDownEvent, Stateful, div, px, relative,
};
use gpui_component::ActiveTheme;

pub(in crate::terminal_tab) mod geometry;

use nmt_terminal::ghostty::ScrollbarInfo;

use crate::terminal_tab::scrollbar::geometry::scrollbar_thumb_geometry;
use crate::terminal_tab::view::TerminalPane;

struct ScrollbarDrag;

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

                    let outcome = this.model.scrollbar_mouse_down(
                        this.local_position(event.position),
                        thumb_top,
                        thumb_height,
                    );

                    if !this.apply_scroll_outcome(outcome, cx) {
                        this.mark_scrollbar_activity(cx);
                    }
                }),
            )
            .on_drag(ScrollbarDrag, |_, _, _, cx| {
                cx.stop_propagation();

                cx.new(|_| Empty)
            })
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ScrollbarDrag>, window, cx| {
                    if this.model.scrollbar.is_dragging() {
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

#[cfg(test)]
mod tests;
