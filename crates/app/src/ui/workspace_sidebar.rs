//! The workspace sidebar: one themed button per workspace with busy/idle
//! indicator and hover-close, plus the new-workspace button and bottom status
//! bar, wrapped in the collapse/expand slide animation. `Sidebar` owns the collapse/width view
//! state; `Shell` holds one and feeds it the workspace summaries to render.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, DragMoveEvent, Entity, KeyDownEvent, ScrollHandle, SharedString,
    StatefulInteractiveElement, Window, div, px, rgb,
};
use gpui_component::animation::Transition;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Icon, IconNamed, Selectable, Sizable, h_flex, v_flex};
use nmt_agent_utils::AgentRuntimeStatus;

use super::{AppSettings, NewWorkspace, Shell};
use crate::ui::codex_usage::CodexUsageView;
use crate::window::WindowRegistry;
use crate::workspace::{WorkspaceId, WorkspaceSummary};

/// Default expanded width of the workspace sidebar, in pixels; the user can
/// drag the right edge to resize.
pub(super) const SIDEBAR_WIDTH: f32 = 180.0;
/// Drag limits: keep the workspace list readable and leave room for the terminal.
pub(super) const MIN_WIDTH: f32 = 140.0;
pub(super) const MAX_WIDTH: f32 = 480.0;

/// Drag payload for the width-resize handle; doubles as the (invisible)
/// drag ghost entity (same pattern as the git sidebar's handle).
#[derive(Clone)]
struct ResizeDrag;

impl Render for ResizeDrag {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Sidebar busy spinner glyph (`assets/icons/loading.svg`), rotated by `Spinner`.
struct LoadingIcon;

impl IconNamed for LoadingIcon {
    fn path(self) -> SharedString {
        "icons/loading.svg".into()
    }
}

/// Sidebar idle indicator glyph (`assets/icons/idle.svg`), a static dot.
struct IdleIcon;

impl IconNamed for IdleIcon {
    fn path(self) -> SharedString {
        "icons/idle.svg".into()
    }
}

/// Sidebar pinned-workspace glyph (`assets/icons/pin.svg`).
struct PinIcon;

impl IconNamed for PinIcon {
    fn path(self) -> SharedString {
        "icons/pin.svg".into()
    }
}

struct WorkspaceDrag {
    from: usize,
}

struct WorkspaceDragPreview {
    label: SharedString,
}

impl Render for WorkspaceDragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(rgb(0x2a2f38))
            .text_color(rgb(0xd8dee9))
            .child(self.label.clone())
    }
}

/// Workspace-sidebar view state: collapse/expand plus the persisted expanded
/// width. Rendered against the workspace summaries the shell passes in.
pub(super) struct Sidebar {
    /// Collapsed (width animates to 0) vs expanded.
    pub(super) collapsed: bool,
    /// False until the first `ToggleSidebar`: the startup render draws the
    /// sidebar at its resting width with no slide-in animation.
    pub(super) animated: bool,
    /// Expanded width in pixels; dragging the right edge adjusts it, persisted
    /// per window in `local_state.toml`.
    pub(super) width: f32,
    scroll: ScrollHandle,
}

impl Sidebar {
    pub(super) fn new(width: f32) -> Self {
        Self {
            collapsed: false,
            animated: false,
            width,
            scroll: ScrollHandle::new(),
        }
    }

    /// One sidebar workspace item: a selectable button with busy indicator,
    /// name/cwd lines, hover-close, and a right-click menu (Rename / Close).
    /// While this workspace is being renamed (`rename` matches its id), the
    /// name line is replaced by the rename input.
    fn render_item(
        &self,
        idx: usize,
        ws: &WorkspaceSummary,
        rename: Option<&(WorkspaceId, Entity<InputState>)>,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        // Busy = spinner, idle = static green dot, vertically centered
        // on the item's left. The SVG fills are flattened by the svg
        // renderer, so the tint is reapplied as a text color.
        let (indicator, status_label): (AnyElement, &'static str) = match ws.agent_status {
            AgentRuntimeStatus::Running => (
                Spinner::new()
                    .small()
                    .icon(Icon::new(LoadingIcon))
                    .color(rgb(0xD36803).into())
                    .into_any_element(),
                "Running",
            ),
            AgentRuntimeStatus::NeedsInput => (
                Icon::new(IdleIcon)
                    .small()
                    .text_color(rgb(0x4A90E2))
                    .into_any_element(),
                "Needs input",
            ),
            AgentRuntimeStatus::Idle => (
                Icon::new(IdleIcon)
                    .small()
                    .text_color(rgb(0x46C878))
                    .into_any_element(),
                "Idle",
            ),
        };
        let indicator = div()
            .id(("workspace-status", idx))
            .aria_label(status_label)
            .child(indicator)
            .into_any_element();
        let ws_id = ws.id;

        // Inline rename: swap the whole item for an indicator + input row.
        // Enter or clicking anywhere else (blur) commits (handled by the
        // shell's subscription on the input); Escape is intercepted here
        // before the input sees it and cancels, keeping the original name.
        if let Some(input) = rename
            .filter(|(id, _)| *id == ws_id)
            .map(|(_, input)| input.clone())
        {
            return h_flex()
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .items_center()
                .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                    if e.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.finish_workspace_rename(false, window, cx);
                    }
                }))
                .child(indicator)
                .child(div().flex_1().child(Input::new(&input).small()))
                .into_any_element();
        }

        let controls: AnyElement = if ws.pinned {
            div()
                .id(("workspace-pin", idx))
                .px_1()
                .invisible()
                .group_hover("ws-item", |this| this.visible())
                .child(Icon::new(PinIcon).small())
                .into_any_element()
        } else if ws.closeable {
            // Hover-only `×` closes the workspace and drops all of its
            // tabs (panes/PTYs die with the dropped Workspace).
            div()
                .id(("workspace-close", idx))
                .px_1()
                .invisible()
                .group_hover("ws-item", |this| this.visible())
                .child("×")
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.request_close_workspace(ws_id, window, cx);
                }))
                .into_any_element()
        } else {
            div().px_1().child("").into_any_element()
        };
        let suffix = h_flex()
            .gap_1()
            .children((ws.unread_count > 0).then(|| {
                div()
                    .id(("workspace-unread", idx))
                    .aria_label(format!("{} unread notifications", ws.unread_count))
                    .px_1()
                    .rounded(px(8.0))
                    .bg(rgb(0x4A90E2))
                    .child(ws.unread_count.to_string())
            }))
            .child(controls);
        let secondary = ws.cwd.clone();
        let drag_label: SharedString = ws.name.clone().into();
        let item = Button::new(("workspace", idx))
            .ghost()
            .selected(ws.active)
            // The ghost-selected token (`secondary.active.background`) sits too
            // close to the sidebar surface in the modern themes; reuse the
            // active-tab pair so the current workspace reads as strongly as the
            // current tab. Button applies these after the variant style.
            .when(ws.active, |this| {
                this.bg(cx.theme().tab_active)
                    .text_color(cx.theme().tab_active_foreground)
            })
            .w_full()
            .h_auto()
            .px_2()
            .py_1()
            .group("ws-item")
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(indicator)
                    .child(
                        v_flex()
                            .flex_1()
                            .overflow_hidden()
                            .items_start()
                            .child(
                                div()
                                    .id(("workspace-secondary", idx))
                                    .aria_label(secondary.clone())
                                    .w_full()
                                    .text_left()
                                    .text_sm()
                                    .truncate()
                                    .child(ws.name.clone()),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .text_left()
                                    .text_xs()
                                    .truncate()
                                    .text_color(cx.theme().sidebar_foreground.opacity(0.6))
                                    .child(secondary),
                            ),
                    )
                    .child(suffix),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.workspaces.activate(idx);
                this.focus_active(window, cx);
                this.sync_session_memory(cx);
                cx.notify();
            }));

        // Right-click menu. Close reuses the same confirm-gated path as the
        // hover `×` (last workspace included: quit/replace/cancel dialog).
        let shell = cx.entity();
        let pinned = ws.pinned;
        let closeable = ws.closeable;
        let pin_label = if pinned { "Unpin" } else { "Pin" };
        div()
            .id(("workspace-menu", idx))
            .w_full()
            .on_drag(WorkspaceDrag { from: idx }, move |_, _, _, cx| {
                cx.new(|_| WorkspaceDragPreview {
                    label: drag_label.clone(),
                })
            })
            .on_drop(cx.listener(move |this, drag: &WorkspaceDrag, window, cx| {
                this.reorder_workspaces(drag.from, idx, window, cx);
            }))
            .context_menu(move |menu, _, _| {
                let rename_shell = shell.clone();
                let close_shell = shell.clone();
                let pin_shell = shell.clone();
                menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                    rename_shell.update(cx, |this, cx| {
                        this.start_workspace_rename(ws_id, window, cx)
                    });
                }))
                .item(PopupMenuItem::new(pin_label).on_click(move |_, _, cx| {
                    pin_shell.update(cx, |this, cx| this.set_workspace_pinned(ws_id, !pinned, cx));
                }))
                .item(PopupMenuItem::new("Close").disabled(!closeable).on_click(
                    move |_, window, cx| {
                        close_shell.update(cx, |this, cx| {
                            this.request_close_workspace(ws_id, window, cx)
                        });
                    },
                ))
            })
            .child(item)
            .into_any_element()
    }

    /// The workspace sidebar: one themed button per workspace (active = selected),
    /// plus a new-workspace button and bottom status bar. Toggled by
    /// `ToggleSidebar` (Ctrl+Shift+B).
    pub(super) fn render(
        &self,
        summaries: Vec<WorkspaceSummary>,
        rename: Option<&(WorkspaceId, Entity<InputState>)>,
        codex_usage: Entity<CodexUsageView>,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        let width = self.width;
        // Fixed-width content; the animated wrapper below clips it so the buttons
        // don't reflow while the sidebar slides. The sidebar surface itself is
        // a floating card — 1px border, large radius, its own background —
        // sitting in a gutter cut from the fixed width, so the drag/animation
        // math keeps operating on the full `width`.
        let card = v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().sidebar_border)
            .rounded(cx.theme().radius_lg)
            .overflow_hidden()
            .p_2()
            .gap_1()
            .child(
                Button::new("new-workspace")
                    .ghost()
                    .label("+ Workspace")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_new_workspace(&NewWorkspace, window, cx)
                    })),
            )
            .child(
                v_flex()
                    .id("workspace-list")
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .relative()
                    .pr_3()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(
                        summaries
                            .iter()
                            .enumerate()
                            .map(|(idx, ws)| self.render_item(idx, ws, rename, cx)),
                    )
                    .child(workspace_list_scrollbar(&self.scroll)),
            )
            .children(cx.global::<AppSettings>().show_agent_usage.then(|| {
                h_flex()
                    .id("workspace-sidebar-status")
                    .w_full()
                    .flex_none()
                    .pt_1()
                    .border_t_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(codex_usage)
            }));
        // Gutter floating the card inside the chrome; no right inset — the
        // terminal column carries its own 6px gutter, which doubles as the gap
        // between the two cards and keeps the resize handle riding the card
        // edge. The top inset matches the tab strip's 4px inset so the card's
        // top edge lines up with the tab pills.
        let content = div()
            .w(px(width))
            .h_full()
            .pl(px(6.))
            .pt(px(4.))
            .pb(px(6.))
            .child(card);

        let collapsed = self.collapsed;
        // Width-resize handle riding the right border: drag starts an
        // (invisible) gpui drag; `on_drag_move` on the wrapper receives the
        // window-level move events and turns the mouse x into a new width.
        // Not rendered while collapsed, so the collapsed sidebar can't resize.
        let resize_handle = (!collapsed).then(|| {
            div()
                .id("workspace-sidebar-resize")
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .w(px(5.0))
                .cursor_col_resize()
                .occlude()
                .hover(|this| this.bg(cx.theme().drag_border))
                .on_drag(ResizeDrag, |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                })
        });
        let wrapper = div()
            .h_full()
            .flex_none()
            .relative()
            .overflow_hidden()
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<ResizeDrag>, _, cx| {
                // The sidebar's left edge is pinned, so the new width is the
                // pointer x minus the left edge, clamped to the drag limits.
                let width = (e.event.position.x - e.bounds.left())
                    .as_f32()
                    .clamp(MIN_WIDTH, MAX_WIDTH);
                if width != this.sidebar.width {
                    this.sidebar.width = width;
                    // Render at the live width; the next toggle re-arms the
                    // slide animation.
                    this.sidebar.animated = false;
                    // Stash in the registry; the quit hook persists it.
                    if let Some(entry) = cx.global_mut::<WindowRegistry>().get_mut(this.window_id) {
                        entry.sidebar_width = Some(width);
                    }
                    cx.notify();
                }
            }))
            .child(content)
            .children(resize_handle);
        // Until the first toggle, render at the resting width — no slide-in on
        // startup.
        if !self.animated {
            let width = if collapsed { px(0.0) } else { px(width) };
            return wrapper.w(width).into_any_element();
        }

        // Slide the sidebar in/out by animating the wrapper width. The id encodes
        // the collapsed state so a toggle restarts the animation from the right
        // end; unrelated re-renders keep the same id and don't re-animate.
        let (from, to) = if collapsed {
            (px(width), px(0.0))
        } else {
            (px(0.0), px(width))
        };
        Transition::new(Duration::from_millis(180))
            .width(from, to)
            .apply(wrapper, ("sidebar", collapsed as usize))
            .into_any_element()
    }
}

fn workspace_list_scrollbar(handle: &ScrollHandle) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(16.0))
        .child(Scrollbar::vertical(handle).scrollbar_show(ScrollbarShow::Always))
}
