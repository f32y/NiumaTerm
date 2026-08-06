use gpui::prelude::*;
use gpui::{App, Div, div};
use gpui_component::ActiveTheme as _;

pub(crate) const SIDE_INSET: f32 = 6.0;
pub(crate) const TOP_INSET: f32 = 4.0;
pub(crate) const BOTTOM_INSET: f32 = 6.0;

/// The shared frame for peer workspace and main-content surfaces.
pub(crate) fn card(cx: &App) -> Div {
    div()
        .size_full()
        .bg(cx.theme().sidebar)
        .border_1()
        .border_color(cx.theme().sidebar_border)
        .rounded(cx.theme().radius_lg)
        .overflow_hidden()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_surface_gutters_share_one_geometry() {
        assert_eq!((SIDE_INSET, TOP_INSET, BOTTOM_INSET), (6.0, 4.0, 6.0));
    }
}
