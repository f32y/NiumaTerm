use gpui::{App, InteractiveElement, MouseButton, MouseDownEvent, MouseUpEvent, Window};

use crate::modern_menu::ModernMenu;

/// Attaches a right-click menu to an element, the counterpart of
/// [`crate::menu::ContextMenuExt`] for menus drawn in a window of their own.
///
/// Unlike that one this adds no wrapper element. A drawn menu has to be anchored
/// and clipped inside the window that opens it, which takes an element to anchor
/// against; a menu with its own window is positioned by the platform, so it only
/// needs mouse handlers.
pub trait ModernMenuExt: InteractiveElement + Sized {
    /// Open a menu built by `builder` when the element is right-clicked.
    ///
    /// `builder` runs on each secondary mouse release, so items reflect the
    /// current state when the menu opens.
    fn modern_context_menu(
        self,
        builder: impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static,
    ) -> Self {
        self.capture_any_mouse_down(move |event: &MouseDownEvent, window, _cx| {
            if event.button != MouseButton::Right {
                return;
            }

            // A secondary press must not activate child controls. Opening on
            // release lets a child link handle its own context menu first.
            window.prevent_default();
        })
        .on_mouse_up(
            MouseButton::Right,
            move |event: &MouseUpEvent, window, cx| {
                cx.stop_propagation();
                builder(ModernMenu::new(), window, cx).show_at(event.position, window, cx);
            },
        )
    }
}

impl<E: InteractiveElement + Sized> ModernMenuExt for E {}
