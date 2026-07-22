use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, Context, DragMoveEvent, ElementId, Entity, KeyDownEvent,
    MouseButton, ScrollHandle, SharedString, StatefulInteractiveElement, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::progress::ProgressCircle;
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{ActiveTheme, Icon, IconNamed, Selectable, Sizable, h_flex, v_flex};
use nmt_agent_utils::AgentRuntimeStatus;

use super::{AppSettings, NewWorkspace, Shell};
use crate::ui::codex_usage::CodexUsageView;
use crate::ui::sidebar_resize::{self, ResizeDrag};
use crate::window::WindowRegistry;
use crate::workspace::{WorkspaceId, WorkspaceSummary};

/// Default expanded width of the workspace sidebar, in pixels; the user can
/// drag the right edge to resize.
pub(super) const SIDEBAR_WIDTH: f32 = 180.0;
/// Drag limits: keep the workspace list readable and leave room for the terminal.
pub(super) const MIN_WIDTH: f32 = 140.0;
pub(super) const MAX_WIDTH: f32 = 480.0;

/// Sidebar idle indicator glyph (`assets/icons/idle.svg`), a static dot.
struct IdleIcon;

impl IconNamed for IdleIcon {
    fn path(self) -> SharedString {
        "icons/idle.svg".into()
    }
}

fn agent_status_indicator(
    status: AgentRuntimeStatus,
    busy_id: impl Into<ElementId>,
) -> (AnyElement, &'static str) {
    match status {
        AgentRuntimeStatus::Running => (
            ProgressCircle::new(busy_id)
                .small()
                .loading(true)
                .color(rgb(0xD36803))
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

/// The floating preview shown under the cursor while dragging a workspace: a
/// full-size replica of the sidebar item (name and cwd lines on the active
/// fill). Its background layers are composited into one opaque color because
/// the ghost leaves the sidebar surface and floats over arbitrary content.
struct WorkspaceDragPreview {
    name: SharedString,
    cwd: SharedString,
    agent_status: AgentRuntimeStatus,
    width: f32,
}

impl Render for WorkspaceDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (indicator, status_label) =
            agent_status_indicator(self.agent_status, "workspace-drag-busy");

        let indicator = div()
            .id("workspace-drag-status")
            .aria_label(status_label)
            .child(indicator);

        let background = cx
            .theme()
            .background
            .blend(cx.theme().sidebar)
            .blend(cx.theme().sidebar_accent);

        h_flex()
            .w(px(self.width))
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .rounded(cx.theme().radius)
            .overflow_hidden()
            .bg(background)
            .text_color(cx.theme().sidebar_accent_foreground)
            .child(indicator)
            .child(
                v_flex()
                    .flex_1()
                    .overflow_hidden()
                    .items_start()
                    .child(
                        div()
                            .w_full()
                            .text_left()
                            .text_sm()
                            .truncate()
                            .child(self.name.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_left()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().sidebar_accent_foreground.opacity(0.6))
                            .child(self.cwd.clone()),
                    ),
            )
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
    /// Item position a workspace drag currently hovers: that item shifts down
    /// to open an insertion gap ("make way"). Only overwritten when the
    /// pointer enters another item — clearing on exit would oscillate, because
    /// opening the gap moves the hovered item out from under the pointer.
    drag_over: Option<usize>,
    /// Source item hidden with zero opacity during a drag so its layout slot
    /// remains stable while the floating preview follows the pointer.
    dragging: Option<usize>,
}

/// How far a workspace item slides down to open the insertion gap while a
/// drag hovers it.
const WS_MAKE_WAY_PX: f32 = 36.0;

impl Sidebar {
    pub(super) fn new(width: f32) -> Self {
        Self {
            collapsed: false,
            animated: false,
            width,
            scroll: ScrollHandle::new(),
            drag_over: None,
            dragging: None,
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
        // Busy = animated progress circle, idle = static SVG dot, vertically
        // centered on the item's left. SVG tint is applied as text color.
        let (indicator, status_label) =
            agent_status_indicator(ws.agent_status, ("workspace-busy", idx));

        let indicator = div()
            .id(("workspace-status", idx))
            .aria_label(status_label)
            .child(indicator)
            .into_any_element();

        let ws_id = ws.id;

        let renaming = rename
            .filter(|(id, _)| *id == ws_id)
            .map(|(_, input)| input.clone());

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
                    .size_5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(2.0))
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .child(ws.unread_count.to_string())
            }))
            .child(controls);

        let secondary = ws.cwd.clone();
        let name = div()
            .id(("workspace-secondary", idx))
            .aria_label(secondary.clone())
            .w_full()
            .text_left()
            .text_sm()
            .truncate();

        let name: AnyElement = if let Some(input) = renaming {
            name.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                    if e.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.finish_workspace_rename(false, window, cx);
                    }
                }))
                .child(
                    Input::new(&input)
                        .xsmall()
                        .p_0()
                        .text_sm()
                        .appearance(false),
                )
                .into_any_element()
        } else {
            name.child(ws.name.clone()).into_any_element()
        };

        let drag_name: SharedString = ws.name.clone().into();
        let drag_cwd: SharedString = ws.cwd.clone().into();
        let drag_agent_status = ws.agent_status;

        // Replicate the item's rendered width: sidebar width minus the card
        // gutter/border and the card's inner paddings around the list.
        let drag_width = (self.width - 36.0).max(80.0);
        let item = Button::new(("workspace", idx))
            .ghost()
            .tooltip(secondary.clone())
            .selected(ws.active)
            // Button resolves selected colors after element styles, so the
            // sidebar-accent pair must be the selected custom variant itself.
            .when(ws.active, |this| {
                this.custom(
                    ButtonCustomVariant::new(cx)
                        .foreground(cx.theme().sidebar_accent_foreground)
                        .active(cx.theme().sidebar_accent),
                )
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
                            .child(name)
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
        let drag_shell = shell.clone();
        let pinned = ws.pinned;
        let closeable = ws.closeable;
        let pin_label = if pinned { "Unpin" } else { "Pin" };
        let cwd = ws.cwd.clone();

        div()
            .id(("workspace-menu", idx))
            .w_full()
            .when(self.dragging == Some(idx), |this| this.opacity(0.0))
            // Make way for the dragged item: the hovered item slides down,
            // opening an insertion gap at the pointer.
            .when(self.drag_over == Some(idx), |this| {
                this.mt(px(WS_MAKE_WAY_PX))
            })
            .on_drag(WorkspaceDrag { from: idx }, move |_, _, _, cx| {
                drag_shell.update(cx, |this, cx| {
                    this.sidebar.dragging = Some(idx);
                    cx.notify();
                });
                cx.new(|_| WorkspaceDragPreview {
                    name: drag_name.clone(),
                    cwd: drag_cwd.clone(),
                    agent_status: drag_agent_status,
                    width: drag_width,
                })
            })
            .on_drag_move(
                cx.listener(move |this, e: &DragMoveEvent<WorkspaceDrag>, _, cx| {
                    if !e.bounds.contains(&e.event.position) {
                        return;
                    }
                    // No gap over the drag's own item: dropping there is a
                    // no-op.
                    let target = (e.drag(cx).from != idx).then_some(idx);
                    if this.sidebar.drag_over != target {
                        this.sidebar.drag_over = target;
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &WorkspaceDrag, window, cx| {
                // The list-level fallback handler must not also reorder this
                // drop.
                cx.stop_propagation();

                this.sidebar.drag_over = None;
                this.sidebar.dragging = None;

                this.reorder_workspaces(drag.from, idx, window, cx);
            }))
            .context_menu(move |menu, _, _| {
                let rename_shell = shell.clone();
                let close_shell = shell.clone();
                let pin_shell = shell.clone();
                let cwd = cwd.clone();

                menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                    rename_shell.update(cx, |this, cx| {
                        this.start_workspace_rename(ws_id, window, cx)
                    });
                }))
                .item(PopupMenuItem::new(pin_label).on_click(move |_, _, cx| {
                    pin_shell.update(cx, |this, cx| this.set_workspace_pinned(ws_id, !pinned, cx));
                }))
                .item(
                    PopupMenuItem::new("Copy Workspace Path").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(cwd.clone()));
                    }),
                )
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
        &mut self,
        summaries: Vec<WorkspaceSummary>,
        rename: Option<&(WorkspaceId, Entity<InputState>)>,
        codex_usage: Entity<CodexUsageView>,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        // Runs every render: close the make-way gap once the drag is gone
        // without a drop on the list (cancelled via Escape, or released
        // elsewhere) — the cancel itself refreshes the window, so this always
        // gets a chance to run.
        if !cx.has_active_drag() {
            self.drag_over = None;
            self.dragging = None;
        }

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
                    // Fallback drop target for the whole list: a drop released
                    // over the make-way gap (a margin, outside every item's
                    // hitbox) still lands on the tracked insertion position
                    // instead of silently ending the drag.
                    .on_drop(cx.listener(|this, drag: &WorkspaceDrag, window, cx| {
                        this.sidebar.dragging = None;

                        if let Some(to) = this.sidebar.drag_over.take() {
                            this.reorder_workspaces(drag.from, to, window, cx);
                        }

                        cx.notify();
                    }))
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

        // Not rendered while collapsed, so the collapsed sidebar can't resize.
        let resize_handle = (!collapsed)
            .then(|| sidebar_resize::resize_handle("workspace-sidebar-resize", false, cx));

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
        sidebar_resize::slide_width(wrapper, "sidebar", !collapsed, px(width), self.animated)
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
