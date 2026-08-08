use std::collections;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DragMoveEvent, Entity, KeyDownEvent, MouseButton, Render,
    ScrollHandle, SharedString, Window, div, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu as _, PopupMenuItem};
use gpui_component::progress::ProgressCircle;
use gpui_component::tab::{Tab, TabBar, TabVariant};
use gpui_component::{ActiveTheme, Sizable};
use nmt_terminal::event::{ProgressReport, ProgressState};

use super::Shell;
use super::shell::TabSurface;
use crate::tabs::{TabId, TabManager};
use crate::ui::AppSettings;

struct TabDrag {
    from: usize,
}

/// The floating preview shown under the cursor while dragging a tab: a
/// full-size replica of the active tab pill. The pill's alpha fill is
/// composited onto the chrome background here, because the ghost floats over
/// arbitrary content where a bare alpha fill would wash out.
struct TabDragPreview {
    label: SharedString,
    width: f32,
}

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .rounded(cx.theme().radius)
            .bg(cx.theme().background)
            .child(
                div()
                    .w(px(self.width))
                    .h(px(30.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().tab_active)
                    .text_sm()
                    .text_color(cx.theme().tab_active_foreground)
                    .child(div().truncate().child(self.label.clone())),
            )
    }
}

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
    /// Tab position a tab drag currently hovers: that tab shifts right to open
    /// an insertion gap ("make way"). Only overwritten when the pointer enters
    /// another tab — clearing on exit would oscillate, because opening the gap
    /// moves the hovered tab out from under the pointer.
    drag_over: Option<usize>,
}

/// How far a tab slides to open the insertion gap while a drag hovers it.
const TAB_MAKE_WAY_PX: f32 = 32.0;

/// One tab's render inputs, snapshotted out of the manager before the closure
/// borrows the shell.
struct TabItem {
    id: u64,
    label: String,
    unread: bool,
    busy: bool,
    bell: bool,
    /// Restored but not yet spawned.
    pending: bool,
    exited: bool,
    progress: Option<ProgressReport>,
}

/// Progress bar along the bottom edge of a tab, driven by OSC 9;4. Sits inside
/// the tab's padding box, so it spans the label rather than the full pill.
fn progress_bar(report: ProgressReport, cx: &App) -> AnyElement {
    let percent = |default: u8| report.progress.unwrap_or(default) as f32 / 100.0;

    let (color, fraction) = match report.state {
        ProgressState::Set => (cx.theme().primary, percent(0)),
        ProgressState::Error => (cx.theme().danger, percent(100)),
        ProgressState::Pause => (cx.theme().warning, percent(100)),
        // Indeterminate reports carry no percentage: a full-width muted bar
        // reads as "running, no ETA" and stays distinguishable from a finished
        // determinate bar, which is full-width in the accent color. No pulse
        // animation — the strip would then repaint every frame for as long as
        // any background command runs.
        ProgressState::Indeterminate => (cx.theme().muted_foreground, 1.0),
        ProgressState::Remove => (cx.theme().primary, 0.0),
    };

    div()
        .absolute()
        .bottom_0()
        .left_0()
        .h(px(2.0))
        .w(relative(fraction))
        .rounded_full()
        .bg(color)
        .into_any_element()
}

impl TabStrip {
    pub(super) fn new() -> Self {
        Self {
            scroll: ScrollHandle::new(),
            last_active: None,
            reveal_retry: false,
            drag_over: None,
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

        // Runs every render: close the make-way gap once the drag is gone
        // without a drop on the strip (cancelled via Escape, or released
        // elsewhere) — the cancel itself refreshes the window, so this always
        // gets a chance to run.
        if self.drag_over.is_some() && !cx.has_active_drag() {
            self.drag_over = None;
        }
    }

    pub(super) fn render(
        &self,
        tabs: &TabManager<TabSurface>,
        unread_tabs: &collections::HashSet<TabId>,
        busy_agent_tabs: &collections::HashSet<TabId>,
        rename: Option<&(TabId, Entity<InputState>)>,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        let active_idx = tabs.active_index();

        let items: Vec<TabItem> = tabs
            .tabs()
            .iter()
            .map(|tab| TabItem {
                id: tab.id().0,
                label: if tab.title().is_empty() {
                    "PowerShell".to_string()
                } else {
                    tab.title().to_string()
                },
                unread: unread_tabs.contains(&tab.id()),
                busy: busy_agent_tabs.contains(&tab.id()),
                bell: tab.bell(),
                pending: matches!(tab.surface(), TabSurface::Pending(_)),
                exited: tab.exited(),
                progress: tab.progress(),
            })
            .collect();

        // `+` right after the last tab opens the new-tab menu: one entry per
        // configured terminal profile, plus one per agent profile.
        // Ctrl+Shift+T still opens the default profile directly.
        let menu_shell = cx.entity();
        let new_tab = Button::new("tab-new")
            .ghost()
            .px_2()
            .child("+")
            .dropdown_menu(move |menu, _, cx| {
                let mut menu = menu;

                for profile in cx.global::<AppSettings>().profiles.clone() {
                    let shell_cmd = profile.shell.trim().to_string();

                    // A profile without a command cannot spawn; offering it
                    // would silently fall back to the built-in shell.
                    if shell_cmd.is_empty() {
                        continue;
                    }

                    let args: Vec<String> = profile
                        .args
                        .split_whitespace()
                        .map(str::to_string)
                        .collect();
                    let item_shell = menu_shell.clone();

                    menu = menu.item(PopupMenuItem::new(profile.name.clone()).on_click(
                        move |_, window, cx| {
                            let launch = (Some(shell_cmd.clone()), args.clone());
                            item_shell
                                .update(cx, |this, cx| this.open_profile_tab(launch, window, cx));
                        },
                    ));
                }

                let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();

                // No separator over an empty agent section (every agent
                // profile deleted).
                if !agent_profiles.is_empty() {
                    menu = menu.separator();
                }

                for (ix, profile) in agent_profiles.into_iter().enumerate() {
                    let label = if profile.name.trim().is_empty() {
                        format!("Agent Profile {}", ix + 1)
                    } else {
                        profile.name.clone()
                    };
                    let item_shell = menu_shell.clone();

                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        let profile = profile.clone();
                        item_shell.update(cx, |this, cx| this.open_agent_tab(profile, window, cx));
                    }));
                }

                menu
            });

        let tab_count = items.len();
        let closeable = tab_count > 1;
        let shell = cx.entity();

        // Fixed width from the Appearance setting; long titles clip inside
        // the tab's own overflow_hidden.
        let tab_width = cx.global::<AppSettings>().tab_width as f32;

        let bar = TabBar::new("shell-tabs")
            // Soft-rounded pills floating on the chrome (VS Code Modern UI
            // look); Large gives a 30px strip, taller than the compact 24px one
            // for an easier click/drag target while leaving the terminal below
            // its room.
            .with_variant(TabVariant::Modern)
            .large()
            .w_full()
            .min_w_0()
            .selected_index(active_idx)
            // Overflowing tabs scroll horizontally; the handle lets the shell
            // scroll the active tab into view on switches.
            .track_scroll(&self.scroll)
            .inline_suffix(new_tab)
            .children(items.into_iter().enumerate().map(|(index, item)| {
                let TabItem {
                    id,
                    label,
                    unread,
                    busy,
                    bell,
                    pending,
                    exited,
                    progress,
                } = item;
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
                // The divider shares the suffix's fixed space so adding visual
                // separation never reduces the room available to the title.
                let suffix = div()
                    .relative()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(close)
                    .when(index + 1 < tab_count, |this| {
                        this.child(
                            div()
                                .absolute()
                                .right_0()
                                .top(px(7.0))
                                .bottom(px(7.0))
                                .w(px(1.0))
                                .bg(cx.theme().border.opacity(0.45)),
                        )
                    });

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
                        .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                            if e.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.finish_tab_rename(false, window, cx);
                            }
                        }))
                        .child(
                            Input::new(&input)
                                .small()
                                .p_0()
                                .text_center()
                                .appearance(false),
                        )
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
                        .gap_1()
                        // Anchors the progress bar to this tab's bottom
                        // edge.
                        .relative()
                        // Restored-but-not-yet-spawned tabs render
                        // faded, the same "sleeping tab" cue browsers
                        // use for discarded tabs. Fading costs no width,
                        // which matters because the tab width is fixed
                        // and any badge would eat into the title.
                        .when(pending, |this| this.opacity(0.6))
                        // A dead shell recolors the title instead of
                        // appending "[exited]", which spent six
                        // characters of a fixed-width tab on state that
                        // a color carries for free.
                        .when(exited, |this| this.text_color(cx.theme().danger))
                        .child(div().truncate().child(label.clone()))
                        // Unread-notification dot: a filled accent
                        // circle instead of a text bullet, so it stays
                        // visible against the muted inactive-tab text.
                        .children(unread.then(|| {
                            div()
                                .flex_none()
                                .size(px(6.0))
                                .rounded_full()
                                .bg(cx.theme().primary)
                        }))
                        // Bell dot, in the warning color so it reads
                        // apart from the unread dot when a tab has
                        // both.
                        .children(bell.then(|| {
                            div()
                                .flex_none()
                                .size(px(6.0))
                                .rounded_full()
                                .bg(cx.theme().warning)
                        }))
                        .children(progress.map(|report| progress_bar(report, cx)))
                        .into_any_element()
                };
                let drag_label: SharedString = label.into();
                Tab::new()
                    .w(px(tab_width))
                    // Make way for the dragged tab: the hovered tab
                    // slides right, opening an insertion gap at the
                    // pointer.
                    .when(self.drag_over == Some(index), |this| {
                        this.ml(px(TAB_MAKE_WAY_PX))
                    })
                    .when(busy, |this| {
                        this.prefix(
                            div()
                                .id(("tab-agent-busy", id as usize))
                                .aria_label("Agent busy")
                                .relative()
                                .left(px(4.0))
                                .size_4()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    ProgressCircle::new(("tab-agent-busy-spinner", id as usize))
                                        .small()
                                        .loading(true)
                                        .color(cx.theme().warning),
                                ),
                        )
                    })
                    .child(content)
                    .suffix(suffix)
                    .group("shell-tab")
                    // Drag a tab to reorder it; drop maps the source position
                    // (`from`) onto this tab's position.
                    .on_drag(TabDrag { from: index }, move |_, _, _, cx| {
                        cx.new(|_| TabDragPreview {
                            label: drag_label.clone(),
                            width: tab_width,
                        })
                    })
                    .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<TabDrag>, _, cx| {
                        if !e.bounds.contains(&e.event.position) {
                            return;
                        }

                        // No gap over the drag's own tab: dropping
                        // there is a no-op.
                        let target = (e.drag(cx).from != index).then_some(index);

                        if this.tab_strip.drag_over != target {
                            this.tab_strip.drag_over = target;
                            cx.notify();
                        }
                    }))
                    .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                        // The strip-level fallback handler must not
                        // also reorder this drop.
                        cx.stop_propagation();

                        this.tab_strip.drag_over = None;
                        this.workspaces.active_tabs_mut().reorder(drag.from, index);

                        this.focus_active(window, cx);
                        this.sync_session_memory(cx);

                        cx.notify();
                    }))
            }))
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                this.workspaces.active_tabs_mut().activate(*ix);

                this.focus_active(window, cx);
                this.sync_session_memory(cx);

                cx.notify();
            }));
        // Fallback drop target for the whole strip: a drop released over the
        // make-way gap (a margin, outside every tab's hitbox) still lands on
        // the tracked insertion position instead of silently ending the drag.
        div()
            .id("tab-strip-drop")
            .w_full()
            .min_w_0()
            .on_drop(cx.listener(|this, drag: &TabDrag, window, cx| {
                if let Some(to) = this.tab_strip.drag_over.take() {
                    this.workspaces.active_tabs_mut().reorder(drag.from, to);

                    this.focus_active(window, cx);
                    this.sync_session_memory(cx);
                }
                cx.notify();
            }))
            .child(bar)
            .into_any_element()
    }
}
