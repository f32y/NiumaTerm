use gpui::{ElementId, Styled as _, px};
use gpui_component::button::{Button, ButtonVariants as _, Toggle, ToggleVariants as _};

use crate::ui::composition::TOOLBAR_BUTTON_SIZE;

/// Standalone icon controls keep the same visible and clickable square.
pub(crate) fn toolbar_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).ghost().size(px(TOOLBAR_BUTTON_SIZE)).p_0()
}

/// A toggle keeps a square icon target and can grow for an optional count.
pub(crate) fn toolbar_toggle(id: impl Into<ElementId>) -> Toggle {
    Toggle::new(id)
        .ghost()
        .min_w(px(TOOLBAR_BUTTON_SIZE))
        .h(px(TOOLBAR_BUTTON_SIZE))
        .px(px(6.))
}
