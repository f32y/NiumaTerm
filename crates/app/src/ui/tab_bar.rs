//! The workspace's tab strip: gpui-component `TabBar` with hover-close,
//! a `+` new-tab suffix, drag-to-reorder, and a right-click menu
//! (Rename / Close) with inline renaming. `TabStrip` owns the strip's scroll
//! position and active-tab reveal tracker; `Shell` holds one and feeds it the
//! active workspace's tabs to render.

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, KeyDownEvent, MouseButton, Render, ScrollHandle, SharedString,
    Window, div, px, rgb,
};
use gpui_component::Sizable;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::tab::{Tab, TabBar, TabVariant};

use super::shell::TerminalPaneTree;
use super::{NewTab, Shell};
use crate::tabs::{TabId, TabManager};
use crate::ui::AppSettings;

/// Drag payload for tab reorder: the source tab's position.
struct TabDrag {
    from: usize,
}

/// The small floating preview shown under the cursor while dragging a tab.
struct TabDragPreview {
    label: SharedString,
}

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(gpui::px(4.0))
            .bg(rgb(0x2a2f38))
            .text_color(rgb(0xd8dee9))
            .child(self.label.clone())
    }
}

/// Tab-strip view state: horizontal scroll position plus the active-tab reveal
/// tracker. Rendered against the active workspace's tabs the shell passes in.
pub(super) struct TabStrip {
    /// Scroll position of the tab strip (tabs overflow horizontally once their
    /// fixed widths exceed the bar).
    pub(super) scroll: ScrollHandle,
    /// Active tab at the last render; a change scrolls the new active tab into
    /// view (render-time compare-and-set, so every switch path counts).
    last_active: Option<TabId>,
    /// True right after the startup reveal request; the request is repeated on
    /// the second render because the scroll handle drops requests made before
    /// its first prepaint.
    reveal_retry: bool,
}

impl TabStrip {
    pub(super) fn new() -> Self {
        Self {
            scroll: ScrollHandle::new(),
            last_active: None,
            reveal_retry: false,
        }
    }

    /// Scroll the newly active tab into view on a switch. Any switch path
    /// re-renders the shell, so this render-time compare-and-set catches every
    /// path without fighting manual scrolling on unrelated re-renders. On the
    /// very first render the scroll handle hasn't recorded its overflow axes yet
    /// (that happens in its first prepaint), so the request is consumed as a
    /// no-op — re-request once on the second render via `reveal_retry`.
    pub(super) fn reveal_active(
        &mut self,
        active_id: TabId,
        active_index: usize,
        cx: &mut Context<Shell>,
    ) {
        let changed = self.last_active != Some(active_id);
        if changed || self.reveal_retry {
            self.reveal_retry = changed && self.last_active.is_none();
            self.last_active = Some(active_id);
            self.scroll.scroll_to_item(active_index);
            if self.reveal_retry {
                cx.notify();
            }
        }
    }

    /// The workspace's tab strip, built from the vendored gpui-component `TabBar`
    /// so it follows the component theme. Clicking a tab activates it; new/close
    /// come via shortcuts. `rename`, when it matches a tab id, swaps that tab's
    /// label for an inline input.
    pub(super) fn render(
        &self,
        tabs: &TabManager<TerminalPaneTree>,
        unread_tabs: &std::collections::HashSet<TabId>,
        rename: Option<&(TabId, Entity<InputState>)>,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        let active_idx = tabs.active_index();
        let items: Vec<(u64, String, bool)> = tabs
            .tabs()
            .iter()
            .map(|tab| {
                let mut label = if tab.title().is_empty() {
                    "PowerShell".to_string()
                } else {
                    tab.title().to_string()
                };
                if tab.exited() {
                    label.push_str(" [exited]");
                }
                let unread = unread_tabs.contains(&tab.id());
                (tab.id().0, label, unread)
            })
            .collect();

        // `+` right after the last tab opens a new tab, same path as
        // Ctrl+Shift+T.
        let new_tab = div()
            .id("tab-new")
            .px_2()
            .cursor_pointer()
            .child("+")
            .on_click(cx.listener(|this, _, window, cx| {
                this.on_new_tab(&NewTab, window, cx);
            }));

        let closeable = items.len() > 1;
        let shell = cx.entity();
        // Fixed width from the Appearance setting; long titles clip inside
        // the tab's own overflow_hidden.
        let tab_width = cx.global::<AppSettings>().tab_width as f32;
        TabBar::new("shell-tabs")
            // Soft-rounded pills floating on the chrome (VS Code Modern UI
            // look); Small keeps the strip compact above the terminal card.
            .with_variant(TabVariant::Modern)
            .small()
            .w_full()
            .min_w_0()
            .selected_index(active_idx)
            // Overflowing tabs scroll horizontally; the handle lets the shell
            // scroll the active tab into view on switches.
            .track_scroll(&self.scroll)
            .inline_suffix(new_tab)
            .children(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(index, (id, mut label, unread))| {
                        if unread {
                            label.push_str("  •");
                        }
                        // `×` suffix closes this tab; `stop_propagation` keeps the click
                        // from also activating the tab (the TabBar's on_click). Shown
                        // only while the tab is hovered (visibility keeps its width, so
                        // the tab doesn't reflow).
                        let close = div()
                            .id(("tab-close", id as usize))
                            .px_1()
                            .invisible()
                            .group_hover("shell-tab", |this| this.visible())
                            .child("×")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.request_close_tab(TabId(id), window, cx);
                            }));
                        // Inline rename: the label swaps for an input. The mouse-down
                        // stopper keeps clicks in the input from activating the tab
                        // (and blurring the input); Escape cancels before the input
                        // sees it. Otherwise the label carries the right-click menu.
                        let renaming = rename
                            .filter(|(rid, _)| *rid == TabId(id))
                            .map(|(_, input)| input.clone());
                        let content: AnyElement = if let Some(input) = renaming {
                            div()
                                .flex_1()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .capture_key_down(cx.listener(
                                    |this, e: &KeyDownEvent, window, cx| {
                                        if e.keystroke.key == "escape" {
                                            cx.stop_propagation();
                                            this.finish_tab_rename(false, window, cx);
                                        }
                                    },
                                ))
                                .child(Input::new(&input).small())
                                .into_any_element()
                        } else {
                            // Right-click menu; Close reuses the confirm-gated path of
                            // the hover `×` and is disabled for the last tab, which
                            // the manager would refuse to close anyway.
                            let menu_shell = shell.clone();
                            div()
                                .id(("tab-menu", id as usize))
                                // Fill the tab body so the whole tab is right-clickable,
                                // keeping the label centered and clipped with ellipsis.
                                .flex_1()
                                .h_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .overflow_hidden()
                                .context_menu(move |menu, _, _| {
                                    let rename_shell = menu_shell.clone();
                                    let close_shell = menu_shell.clone();
                                    menu.item(PopupMenuItem::new("Rename").on_click(
                                        move |_, window, cx| {
                                            rename_shell.update(cx, |this, cx| {
                                                this.start_tab_rename(TabId(id), window, cx)
                                            });
                                        },
                                    ))
                                    .item(
                                        PopupMenuItem::new("Close").disabled(!closeable).on_click(
                                            move |_, window, cx| {
                                                close_shell.update(cx, |this, cx| {
                                                    this.request_close_tab(TabId(id), window, cx)
                                                });
                                            },
                                        ),
                                    )
                                })
                                .child(div().truncate().child(label.clone()))
                                .into_any_element()
                        };
                        let drag_label: SharedString = label.into();
                        Tab::new()
                            .w(px(tab_width))
                            .child(content)
                            .suffix(close)
                            .group("shell-tab")
                            // Drag a tab to reorder it; drop maps the source position
                            // (`from`) onto this tab's position.
                            .on_drag(TabDrag { from: index }, move |_, _, _, cx| {
                                cx.new(|_| TabDragPreview {
                                    label: drag_label.clone(),
                                })
                            })
                            .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                                this.workspaces.active_tabs_mut().reorder(drag.from, index);
                                this.focus_active(window, cx);
                                this.sync_session_memory(cx);
                                cx.notify();
                            }))
                    }),
            )
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                this.workspaces.active_tabs_mut().activate(*ix);
                this.focus_active(window, cx);
                this.sync_session_memory(cx);
                cx.notify();
            }))
            .into_any_element()
    }
}
