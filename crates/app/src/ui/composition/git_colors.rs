use gpui::{App, Hsla, rgb};
use gpui_component::ActiveTheme;

pub(crate) struct GitColors {
    pub(crate) added: Hsla,
    pub(crate) removed: Hsla,
    pub(crate) added_background: Hsla,
    pub(crate) removed_background: Hsla,
    pub(crate) added_word: Hsla,
    pub(crate) removed_word: Hsla,
}

impl GitColors {
    pub(crate) fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let dark = theme.mode.is_dark();

        let added = theme.green;
        let removed = theme.red;

        // Review fills have their own semantic palette: terminal ANSI colors
        // vary too widely to produce a consistent row contrast by blending.
        let (added_background, removed_background, added_word, removed_word) = if dark {
            (0x243d2d, 0x482c30, 0x355c40, 0x713e46)
        } else {
            (0xe1fce1, 0xfee4e3, 0xb3e8b3, 0xf5b9b7)
        };

        Self {
            added,
            removed,
            added_background: rgb(added_background).into(),
            removed_background: rgb(removed_background).into(),
            added_word: rgb(added_word).into(),
            removed_word: rgb(removed_word).into(),
        }
    }
}
