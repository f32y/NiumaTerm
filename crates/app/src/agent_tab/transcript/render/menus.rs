//! The right-click menus transcript rows carry.

use gpui::{App, ClipboardItem, WeakEntity, Window};
use gpui_component::IconName;
use gpui_component::modern_menu::ModernMenu;
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::composer::PromptTarget;
use crate::agent_tab::transcript::{TranscriptView, entry_copy_text};

/// The row menu that copies entry `index` of `view`'s conversation.
pub(crate) fn copy_entry_menu(
    view: WeakEntity<TranscriptView>,
    index: usize,
) -> impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static {
    move |menu, _, cx| {
        // Full transcript payloads can be very large. Resolve and clone the
        // text only after a right click opens the menu, keeping ordinary
        // list layout independent of the hidden message size.
        let copy_text = view
            .read_with(cx, |view, _| {
                view.conversation
                    .borrow()
                    .content
                    .entries()
                    .get(index)
                    .map(|entry| entry_copy_text(&entry.item))
            })
            .ok()
            .flatten();

        match copy_text {
            Some(copy_text) => menu
                .item(t!("agent-transcript-copy"), move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                })
                .icon(IconName::Copy),
            None => menu,
        }
    }
}

/// The copy item plus the actions a prompt offers over the conversation
/// it opened: branching in front of it, or returning to it.
///
/// Which of the two appears follows the backend. Where a branch is a
/// request the harness answers, the prompt names a cut and nothing else;
/// where the conversation is a transcript file this side rewrites, the
/// same cut also decides what happens to the files that turn touched, so
/// the rewind actions are what the prompt leads to.
///
/// `target` is the pane that owns the conversation and the prompt's place in
/// it, absent where the backend offers neither action. `session_fork` picks
/// branching over rewinding.
pub(crate) fn prompt_row_menu(
    view: WeakEntity<TranscriptView>,
    index: usize,
    session_fork: bool,
    target: Option<(WeakEntity<AgentPane>, PromptTarget)>,
) -> impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static {
    let copy = copy_entry_menu(view, index);

    move |menu, window, cx| {
        let menu = copy(menu, window, cx);

        let Some((pane, target)) = target.clone() else {
            return menu;
        };

        if session_fork {
            menu.separator()
                .item(t!("agent-transcript-fork-from-here"), move |_, cx| {
                    let target = target.clone();

                    pane.update(cx, |pane, cx| pane.fork_from_prompt(target, cx))
                        .ok();
                })
                .icon(IconName::GitBranch)
        } else {
            menu.separator()
                .item(t!("agent-transcript-rewind-to-here"), move |_, cx| {
                    let target = target.clone();

                    pane.update(cx, |pane, cx| pane.rewind_to_prompt(target, cx))
                        .ok();
                })
                .icon(IconName::Undo)
        }
    }
}
