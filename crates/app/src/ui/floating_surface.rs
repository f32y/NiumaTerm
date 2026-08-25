use gpui::prelude::*;
use gpui::{App, Div, div};
use gpui_component::ActiveTheme as _;

use crate::ui::UI_RADIUS;

/// Frame for the main terminal or Agent surface. The surface runs into the
/// window's right and bottom edges, so it is framed only where it actually
/// borders other chrome: a left edge against the sidebar gutter, a top edge
/// under the tab strip, and a single rounded corner between them. Drawing a
/// border or radius on the other two sides would trace a line just inside the
/// window frame.
pub(crate) fn card(cx: &App) -> Div {
    div()
        .size_full()
        .overflow_hidden()
        .border_l_1()
        .border_t_1()
        .border_color(cx.theme().sidebar_border)
        .rounded_tl(UI_RADIUS)
        .bg(cx.theme().background)
}
