use gpui::{App, Hsla, rgb};
use gpui_component::ActiveTheme;

pub(crate) struct GitColors {
    pub(crate) added: Hsla,
    pub(crate) removed: Hsla,
    pub(crate) added_background: Hsla,
    pub(crate) removed_background: Hsla,
}

impl GitColors {
    pub(crate) fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let dark = theme.mode.is_dark();
        let added = Hsla {
            s: theme.green.s * 0.7,
            l: if dark { 0.66 } else { 0.48 },
            ..theme.green
        };
        let removed = Hsla {
            s: theme.red.s * 0.7,
            l: if dark { 0.70 } else { 0.58 },
            ..theme.red
        };
        Self {
            added,
            removed,
            added_background: if dark {
                Hsla { l: 0.20, ..added }
            } else {
                rgb(0xe1fce1).into()
            },
            removed_background: if dark {
                Hsla { l: 0.22, ..removed }
            } else {
                rgb(0xfee5e4).into()
            },
        }
    }
}
