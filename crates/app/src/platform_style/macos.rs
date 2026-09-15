use gpui::{App, Div, Hsla};

use crate::platform_style::PlatformStyle;

pub(crate) struct MacOs;

impl PlatformStyle for MacOs {
    /// The history list sits directly on the pane surface, without a frame or
    /// a tint of its own.
    fn history_strip(strip: Div, _background: Hsla, _cx: &App) -> Div {
        strip
    }
}
