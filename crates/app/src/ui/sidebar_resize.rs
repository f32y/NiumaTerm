//! Pieces shared by the two resizable slide-in sidebars (git, workspace):
//! the width-resize drag handle and the open/close width transition.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Pixels, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::animation::Transition;

#[derive(Clone)]
pub(crate) struct ResizeDrag;

impl Render for ResizeDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The 5px column-resize handle riding a sidebar edge (`pin_left` picks which
/// edge). Dragging starts an (invisible) gpui drag; the sidebar's wrapper
/// listens for `DragMoveEvent<ResizeDrag>` window-level move events and turns
/// the pointer x into a new width.
pub(crate) fn resize_handle(id: &'static str, pin_left: bool, cx: &App) -> impl IntoElement {
    let handle = div()
        .id(id)
        .absolute()
        .top_0()
        .bottom_0()
        .w(px(5.0))
        .cursor_col_resize()
        .occlude()
        .hover(|this| this.bg(cx.theme().drag_border))
        .on_drag(ResizeDrag, |drag, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| drag.clone())
        });
    if pin_left {
        handle.left_0()
    } else {
        handle.right_0()
    }
}

/// Slide the sidebar wrapper between zero and `width` by animating its width.
/// While `animated` is false, render at the resting width instead (startup and
/// live drag frames). The animation id encodes the open state so a toggle
/// restarts the slide from the right end while unrelated re-renders keep the
/// same id and don't re-animate.
pub(crate) fn slide_width<E: IntoElement + Styled + 'static>(
    wrapper: E,
    id: &'static str,
    open: bool,
    width: Pixels,
    animated: bool,
) -> AnyElement {
    if !animated {
        let width = if open { width } else { px(0.0) };
        return wrapper.w(width).into_any_element();
    }
    let (from, to) = if open {
        (px(0.0), width)
    } else {
        (width, px(0.0))
    };
    Transition::new(Duration::from_millis(180))
        .width(from, to)
        .apply(wrapper, (id, open as usize))
        .into_any_element()
}
