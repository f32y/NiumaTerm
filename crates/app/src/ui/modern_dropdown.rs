use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, IntoElement, ParentElement as _, Pixels, Styled as _, Window, canvas, div,
};
use gpui_component::button::Button;
use gpui_component::modern_menu::ModernMenu;

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
        .child(button.on_click(move |_, window, cx| {
            builder(ModernMenu::new(), window, cx).show_at(
                measured.get().bottom_left(),
                window,
                cx,
            );
        }))
        .child(
            canvas(move |bounds, _, _| recorded.set(bounds), |_, _, _, _| {})
                .absolute()
                .inset_0(),
        )
}
