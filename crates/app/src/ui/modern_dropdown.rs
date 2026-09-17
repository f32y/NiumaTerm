use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels,
    Styled as _, Window, canvas, div, px,
};
use gpui_component::button::Button;
use gpui_component::modern_menu::{ModernMenu, dismiss_modern_menu};

/// A button that opens a modern menu under its own bottom-left corner.
///
/// The menu is drawn in a window of its own, which the platform places from a
/// point rather than from an element, so the button's rectangle has to reach the
/// press that opens the menu. The canvas records it while the frame is laid out;
/// the press reads what the last frame measured, which is where the button was
/// when it was clicked. Its insets are what pin it over the button: an absolute
/// element without them keeps the place in the flow it would have had, which
/// here is a full button-height below the button.
pub(crate) fn modern_dropdown(
    button: Button,
    builder: impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static,
) -> impl IntoElement {
    let measured: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
    let recorded = measured.clone();

    div()
        .flex_none()
        .relative()
        // Titlebar controls block ancestor hitboxes to prevent window dragging,
        // so the shell's menu dismissal cannot see their presses. Dismiss here
        // on mouse down so releasing the button opens the menu again.
        .on_mouse_down(MouseButton::Left, |_, _, cx| dismiss_modern_menu(cx))
        .child(button.on_click(move |_, window, cx| {
            let mut position = measured.get().bottom_left();

            // Native menus position their content, leaving the rounded outer
            // frame above it. Reserve enough space to keep the trigger visible.
            position.y += px(8.);

            builder(ModernMenu::new(), window, cx).show_at(position, window, cx);
        }))
        .child(
            canvas(move |bounds, _, _| recorded.set(bounds), |_, _, _, _| {})
                .absolute()
                .inset_0(),
        )
}
