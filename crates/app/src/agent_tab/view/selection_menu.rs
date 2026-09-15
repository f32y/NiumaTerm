use gpui::{App, ClipboardItem, Pixels, Point, WeakEntity, Window};
use gpui_base::TextSelection;
use gpui_component::modern_menu::ModernMenu;
use gpui_component::{IconName, WindowExt as _};
use rust_i18n::t;

use crate::agent_tab::AgentPane;

/// The menu over transcript text the user just selected: copy it, or quote
/// it into the composer of `pane`. Nothing opens when the release left no
/// selection.
pub(crate) fn show_selected_text_menu(
    pane: WeakEntity<AgentPane>,
    released_at: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let selected_text = TextSelection::selected_text(window, cx).trim().to_string();

    if selected_text.is_empty() {
        return;
    }

    // Anchored on the selection rather than the pointer, and opened above
    // it, so the text the two actions operate on stays visible while the
    // menu is up. The rect is the union of the selected line boxes, so its
    // top-left is above and left of every selected line.
    let anchor = window
        .selected_text_bounds(cx)
        .map_or(released_at, |bounds| bounds.origin);

    let copy_text = selected_text.clone();

    ModernMenu::new()
        // A selection menu offers two actions that are recognised by icon, so
        // the command row reaches them in one horizontal band instead of a
        // stack of labelled rows the pointer has to travel down.
        .commands(|menu| {
            menu.item(t!("agent-transcript-copy"), move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
            })
            .icon(IconName::Copy)
            .item(t!("agent-transcript-quote"), move |window, cx| {
                let selected_text = selected_text.clone();

                let _ = pane.update(cx, |pane, cx| {
                    pane.add_response_annotation(selected_text, window, cx);
                });
            })
            .icon(IconName::TextSelect)
        })
        .show_above(anchor, window, cx);
}
