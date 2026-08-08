use gpui::prelude::*;
use gpui::{App, Div, div};
use gpui_component::ActiveTheme as _;

use crate::ui::UI_RADIUS;

pub(crate) const SIDE_INSET: f32 = 6.0;
pub(crate) const TOP_INSET: f32 = 1.0;
pub(crate) const BOTTOM_INSET: f32 = 6.0;

/// Frame for the main terminal or Agent surface.
pub(crate) fn card(cx: &App) -> Div {
    div()
        .size_full()
        .bg(cx.theme().background)
        .border_1()
        .border_color(cx.theme().sidebar_border)
        .rounded(UI_RADIUS)
        .overflow_hidden()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_surface_gutters_share_one_geometry() {
        assert_eq!((SIDE_INSET, TOP_INSET, BOTTOM_INSET), (6.0, 1.0, 6.0));
    }
}
