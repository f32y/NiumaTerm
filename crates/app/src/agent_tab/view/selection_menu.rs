use gpui::{App, Pixels, Point, WeakEntity, Window};
use gpui_base::TextSelection;
use gpui_component::input::Copy;
use gpui_component::modern_menu::ModernMenu;
use gpui_component::{Icon, IconName, WindowExt as _};
use rust_i18n::t;

use crate::agent_tab::AgentPane;

/// The menu over transcript text the user just selected: copy it, quote it
/// into the composer of `pane`, or draft it as a Side Chat question. Nothing
/// opens when the release left no selection.
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

    // Anchored on the selection, not the pointer, and opened above
    // it, so the text the two actions operate on stays visible while the
    // menu is up. The rect is the union of the selected line boxes, so its
    // top-left is above and left of every selected line.
    let anchor = window
        .selected_text_bounds(cx)
        .map_or(released_at, |bounds| bounds.origin);

    let side_text = selected_text.clone();

    let side_pane = pane.clone();

    let offers_side_chat = pane
        .upgrade()
        .is_some_and(|pane| pane.read(cx).offers_side_chat(cx));

    ModernMenu::new()
        // A selection menu offers a few actions that are recognised by icon,
        // so the command row reaches them in one horizontal band instead of a
        // stack of labelled rows the pointer has to travel down.
        .commands(|menu| {
            let menu = menu
                .action(t!("agent-transcript-copy"), Box::new(Copy))
                .icon(IconName::Copy)
                .item(t!("agent-transcript-quote"), move |window, cx| {
                    let selected_text = selected_text.clone();

                    let _ = pane.update(cx, |pane, cx| {
                        pane.add_response_annotation(selected_text, window, cx);
                    });
                })
                .icon(IconName::TextSelect);

            if !offers_side_chat {
                return menu;
            }

            menu.item(t!("agent-side-chat"), move |window, cx| {
                let side_text = side_text.clone();

                let _ = side_pane.update(cx, |pane, cx| {
                    pane.draft_side_question(side_text, window, cx);
                });
            })
            // The bundled icon set has no speech bubble; this is the asset
            // the title bar's Side Chat toggle draws, so both read the same.
            .icon(Icon::empty().path("icons/message-circle.svg"))
        })
        .show_above(anchor, window, cx);
}
