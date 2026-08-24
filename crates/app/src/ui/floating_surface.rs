use gpui::prelude::*;
use gpui::{App, Div, div};
use gpui_component::{ActiveTheme as _, StyledExt as _};

use crate::ui::composition::surface_frame;

/// Frame for the main terminal or Agent surface.
pub(crate) fn card(cx: &App) -> Div {
    div()
        .refine_style(&surface_frame(cx))
        .size_full()
        .bg(cx.theme().background)
}
