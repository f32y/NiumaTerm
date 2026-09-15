use gpui::{App, Div, Hsla, Styled as _};
use gpui_component::ActiveTheme as _;

use crate::agent_tab::settings::UI_RADIUS;
use crate::platform_style::PlatformStyle;

pub(crate) struct Windows;

impl PlatformStyle for Windows {
    /// A bordered strip of a slightly deeper surface sets the history list
    /// apart from the pane. The tint is composited over the pane rather than
    /// taken at full alpha: Fluent's `muted` is a translucent overlay, so
    /// forcing its alpha to 1 would paint the strip in the tint's bare RGB,
    /// solid black in the light theme and solid white in the dark one.
    fn history_strip(strip: Div, background: Hsla, cx: &App) -> Div {
        strip
            .rounded(UI_RADIUS)
            .border_1()
            .border_color(cx.theme().border.opacity(0.6))
            .bg(background.blend(cx.theme().muted))
    }
}
