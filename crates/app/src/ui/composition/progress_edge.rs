use app::design::SURFACE_RADIUS;
use gpui::prelude::*;
use gpui::{Div, Hsla, div, px, relative};

/// Progress track along the bottom edge of a rounded row, filled to
/// `fraction`. One corner radius of space at each side keeps the track on the
/// straight part of the edge. The row must be `relative`, since the track is
/// placed out of the row's flow.
pub(crate) fn progress_edge(fraction: f32, color: Hsla) -> Div {
    div()
        .absolute()
        .bottom_0()
        .left(SURFACE_RADIUS)
        .right(SURFACE_RADIUS)
        .h(px(2.0))
        .child(
            div()
                .h_full()
                .w(relative(fraction))
                .rounded_full()
                .bg(color),
        )
}
