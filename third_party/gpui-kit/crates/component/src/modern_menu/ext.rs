use gpui::{App, InteractiveElement, MouseButton, MouseDownEvent, Window};

use crate::modern_menu::ModernMenu;

/// Attaches a right-click menu to an element, the counterpart of
/// [`crate::menu::ContextMenuExt`] for menus drawn in a window of their own.
///
/// Unlike that one this adds no wrapper element. A drawn menu has to be anchored
/// and clipped inside the window that opens it, which takes an element to anchor
/// against; a menu with its own window is positioned by the platform, so a press
/// handler is the whole of it.
pub trait ModernMenuExt: InteractiveElement + Sized {
    /// Open a menu built by `builder` when the element is right-clicked.
    ///
    /// `builder` runs on every press rather than once, so items can reflect the
    /// state at the moment the menu is opened.
    fn modern_context_menu(
        self,
        builder: impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static,
    ) -> Self {
        self.on_mouse_down(
            MouseButton::Right,
            move |event: &MouseDownEvent, window, cx| {
                builder(ModernMenu::new(), window, cx).show_at(event.position, window, cx);
            },
        )
    }
}

impl<E: InteractiveElement + Sized> ModernMenuExt for E {}
