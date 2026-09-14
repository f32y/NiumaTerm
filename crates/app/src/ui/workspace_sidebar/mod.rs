mod drag;

#[cfg(test)]
mod tests;

use std::borrow::Cow;

use app::agent_tab::AgentKind;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, DragMoveEvent, ElementId, Entity, FontWeight, Role,
    ScrollHandle, SharedString, div, px, relative,
};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::scroll::Scrollbar;
use gpui_component::tooltip::ManagedTooltipExt as _;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IconNamed, Selectable, Sizable, h_flex, v_flex,
};
use nmt_agent::{AgentProjection, AgentRuntimeStatus};
use nmt_config::appearance::TabBarStyle;
use nmt_terminal::event::ProgressReport;
use rust_i18n::t;

use crate::agent_usage::AgentUsageView;
use crate::tabs::TabId;
use crate::ui::composition::{
    FLOATING_SURFACE_BOTTOM_INSET, FLOATING_SURFACE_SIDE_INSET, FLOATING_SURFACE_TOP_INSET,
    HoverActionLayout, HoverActionVisibility, StatusMark, StatusMarkTone, hover_action,
    sidebar_selection, toolbar_button,
};
use crate::ui::fluent::{SELECTION_BAR_HEIGHT, SELECTION_BAR_RADIUS, SELECTION_BAR_WIDTH};
use crate::ui::shell::{
    InlineRename, InlineRenameSession, InlineRenameStyle, MIN_SIDEBAR_WIDTH, pending_tab_icon,
};
use crate::ui::sidebar_resize::ResizeDrag;
use crate::ui::tab_bar::{
    DragLabelPreview, DragStyle, TAB_ROW_HEIGHT, new_tab_menu, progress_visual,
};
use crate::ui::terminal_status::{terminal_dot, terminal_presentation};
use crate::ui::token_usage::TokenUsageView;
use crate::ui::workspace_sidebar::drag::{SidebarTabDrag, WorkspaceDrag, WorkspaceDragPreview};
use crate::ui::{AppSettings, NewWorkspace, Shell, UI_RADIUS, modern_dropdown, sidebar_resize};
#[cfg(target_os = "macos")]
use crate::window::TRAFFIC_LIGHT_INSET;
use crate::window::WindowRegistry;
use crate::workspace::{ProgressTally, TerminalActivity, WorkspaceKind, WorkspaceSummary};

pub(super) struct WorkspaceChrome {
    pub summary: WorkspaceSummary,
    pub agent: AgentProjection,
    pub terminal_activity: TerminalActivity,
    pub progress: ProgressTally,
}

/// Default expanded width of the workspace sidebar, in pixels; the user can
/// drag the right edge to resize.
pub(super) const SIDEBAR_WIDTH: f32 = 180.0;

/// Drag limits: keep the workspace list readable and leave room for the terminal.
/// Handle id for this column's resize grip. Every resizable column receives
/// every other column's drag-move events, so this is what distinguishes them.
pub(super) const RESIZE_HANDLE: &str = "workspace-sidebar-resize";

pub(super) const MIN_WIDTH: f32 = MIN_SIDEBAR_WIDTH;
pub(crate) const MAX_WIDTH: f32 = 480.0;

/// Diameter of a status dot in the sidebar column. Smaller than the agent
/// spinner's `size_3`, so a stacked pair reads as a spinner with a mark under
/// it rather than as two equal glyphs.
const STATUS_DOT: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentVisual {
    Running,
    NeedsInput,
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

    /// The same make-way/hide pair for tab rows, keyed by workspace position
    /// and row position so rows of different workspaces cannot collide.
    tab_drag_over: Option<(usize, usize)>,

    tab_dragging: Option<(usize, usize)>,
}

impl Sidebar {
    pub(super) fn new(width: f32) -> Self {
        Self {
            collapsed: false,
            animated: false,
            width,
            scroll: ScrollHandle::new(),
            drag_over: None,
            dragging: None,
            tab_drag_over: None,
            tab_dragging: None,
        }
    }

    /// The workspace sidebar: one themed button per workspace (active = selected),
    /// plus a new-workspace button and bottom status bar. Toggled by
    /// `ToggleSidebar` (Ctrl+Shift+B).
    pub(super) fn render(
        &mut self,
        summaries: Vec<WorkspaceChrome>,
        // One entry per summary in the vertical tab-bar style, empty in the
        // horizontal one where the title bar still owns the tabs.
        tabs: Vec<Vec<SidebarTab>>,
        renames: &InlineRenameSession,
        usage: SidebarUsage,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        // Runs every render: close the make-way gap once the drag is gone
        // without a drop on the list (cancelled via Escape, or released
        // elsewhere) — the cancel itself refreshes the window, so this always
        // gets a chance to run.
        if !cx.has_active_drag() {
            self.drag_over = None;
            self.dragging = None;
            self.tab_drag_over = None;
            self.tab_dragging = None;
        }

        let width = self.width;

        let has_temporary_workspaces = summaries
            .iter()
            .any(|workspace| workspace.summary.temporary);

        let show_daily_token_usage = cx
            .global::<AppSettings>()
            .config()
            .appearance
            .show_daily_token_usage;

        let show_agent_usage = cx.global::<AppSettings>().config().agent.show_agent_usage;

        // Fixed-width content; the animated wrapper below clips it so the buttons
        // don't reflow while the sidebar slides. The transparent panel inherits
        // the window background while the drag and animation math keeps operating
        // on the full `width`.
        let panel = div()
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .pl(px(SIDEBAR_PADDING_LEFT))
            .pr(px(SIDEBAR_PADDING_X))
            .gap(px(SIDEBAR_GROUP_GAP))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(
                        div()
                            .ml(px(-SIDEBAR_ROW_GUTTER))
                            .text_size(px(SIDEBAR_SECTION_TEXT))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().sidebar_foreground.opacity(0.5))
                            // Set as a caps label so the heading is told apart
                            // from the workspace names by case rather than by
                            // weight, which the names now use to mark the
                            // active one. Scripts without case are unchanged.
                            .child(t!("sidebar-workspaces-title").to_uppercase()),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                toolbar_button("new-workspace")
                                    .icon(IconName::Plus)
                                    .accessibility_label(t!("shell-workspace-new-title"))
                                    .tooltip(t!("shell-workspace-new-title"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.on_new_workspace(&NewWorkspace, window, cx)
                                    })),
                            )
                            .child(
                                toolbar_button("close-temporary-workspaces")
                                    .icon(CloseTemporaryWorkspacesIcon)
                                    .accessibility_label(t!("sidebar-workspace-close-temporary"))
                                    .tooltip(t!("sidebar-workspace-close-temporary"))
                                    .disabled(!has_temporary_workspaces)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.request_close_temporary_workspaces(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                // The scrollbar sits in this non-scrolling wrapper: an absolute
                // child of the scrolling list would be laid out against the
                // content origin and slide out of the viewport as the list
                // scrolls.
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // Stretch the wrapper over the leading gutter so the list
                    // retains the header controls' trailing edge.
                    .ml(px(-SIDEBAR_ROW_GUTTER))
                    .child(
                        v_flex()
                            .id("workspace-list")
                            .size_full()
                            .gap(px(WORKSPACE_LIST_GAP))
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
                            .on_drop(cx.listener(|this, drag: &SidebarTabDrag, window, cx| {
                                this.sidebar.tab_dragging = None;

                                if let Some((ws, to)) = this.sidebar.tab_drag_over.take()
                                    && drag.workspace == ws
                                {
                                    this.reorder_tab(drag.tab, drag.from, to, window, cx);
                                }

                                cx.notify();
                            }))
                            .children(summaries.iter().enumerate().map(|(idx, ws)| {
                                // A workspace heads its own tab rows, and the
                                // list gap is what separates one such block
                                // from the next; a rule between them would
                                // draw a second boundary inside the same gap.
                                let mut rows = Vec::new();

                                rows.push(self.render_item(idx, ws, renames, cx));

                                let ws_tabs = tabs.get(idx).map(Vec::as_slice).unwrap_or_default();

                                // Closing a workspace's last tab falls through to
                                // closing the workspace, so the row keeps its
                                // control as long as one of the two would take
                                // effect. A pinned or sole workspace refuses both,
                                // and the row withholds a control that would do
                                // nothing.
                                let closeable = ws_tabs.len() > 1 || ws.summary.closeable;

                                rows.extend(ws_tabs.iter().enumerate().map(|(tab_idx, tab)| {
                                    self.render_tab_row(idx, tab_idx, tab, closeable, renames, cx)
                                }));

                                v_flex().w_full().children(rows)
                            })),
                    )
                    .child(workspace_list_scrollbar(&self.scroll)),
            )
            .children((show_daily_token_usage || show_agent_usage).then(|| {
                v_flex()
                    .id("workspace-sidebar-status")
                    .w_full()
                    .flex_none()
                    .gap(px(SIDEBAR_STATUS_ROW_GAP))
                    .pt(px(SIDEBAR_STATUS_PADDING_TOP))
                    .pb(px(SIDEBAR_STATUS_PADDING_BOTTOM))
                    .border_t_1()
                    .border_color(cx.theme().sidebar_border)
                    .children(show_daily_token_usage.then_some(usage.daily))
                    .children(
                        show_agent_usage
                            .then(|| div().ml(px(-SIDEBAR_ROW_GUTTER)).child(usage.quotas)),
                    )
            }));

        // The terminal column's left gutter forms the gap between panels and
        // keeps the resize handle at the panel edge, so no right inset is needed.
        let content = div()
            .w(px(width))
            .h_full()
            .pl(px(FLOATING_SURFACE_SIDE_INSET))
            .pt(px(FLOATING_SURFACE_TOP_INSET))
            .pb(px(FLOATING_SURFACE_BOTTOM_INSET))
            .child(panel);

        let collapsed = self.collapsed;

        // Not rendered while collapsed, so the collapsed sidebar can't resize.
        let resize_handle =
            (!collapsed).then(|| sidebar_resize::resize_handle(RESIZE_HANDLE, false, cx));

        let wrapper = div()
            .h_full()
            .flex_none()
            .relative()
            .overflow_hidden()
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<ResizeDrag>, _, cx| {
                // The panel on the other side drags the same type, and these
                // events carry no bounds test, so a gesture that did not start
                // here would otherwise resize this column too.
                if !e.drag(cx).is_from(RESIZE_HANDLE) {
                    return;
                }

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

    /// One sidebar workspace item: a selectable button with busy indicator,
    /// name/cwd lines, hover-close, and a right-click menu (Rename / Close).
    /// While this workspace is being renamed (`rename` matches its id), the
    /// name line is replaced by the rename input.
    fn render_item(
        &self,
        idx: usize,
        chrome: &WorkspaceChrome,
        renames: &InlineRenameSession,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        let ws = &chrome.summary;

        let settings_entry = ws.kind == WorkspaceKind::Settings;
        let selection = sidebar_selection(cx);

        // In the vertical tab-bar style every tab of this workspace is on
        // screen as its own row carrying its own status mark and progress, so
        // the workspace's aggregate of them would say the same thing twice.
        let vertical_tabs =
            cx.global::<AppSettings>().config().appearance.tab_bar_style == TabBarStyle::Vertical;

        let highlight_active = ws.active && !vertical_tabs;

        let (glyphs, status_label) = workspace_status_glyphs(
            chrome.agent.status,
            chrome.terminal_activity,
            ("workspace-busy", idx),
            cx,
        );

        // Runtime marks share the trailing controls so names keep a stable
        // leading edge in both tab layouts, including idle workspaces.
        let indicator = (!vertical_tabs).then(|| {
            v_flex()
                .id(("workspace-status", idx))
                // The column's width is fixed so an idle workspace can suppress
                // its glyphs without shifting its name relative to active
                // neighbours; the height follows its contents so a stacked pair
                // centers as a group and a lone glyph centers on its own.
                .w_4()
                .flex_none()
                .gap_0p5()
                .items_center()
                .justify_center()
                .aria_label(status_label.clone())
                .children(glyphs)
                .into_any_element()
        });

        let ws_id = ws.id;

        let renaming = renames.workspace_input(ws_id).cloned();

        let controls: AnyElement = if vertical_tabs && !settings_entry {
            // This row heads the workspace's own tab list here, so its control
            // adds a tab to that list; closing moves to the context menu. The
            // press activates the workspace before the menu opens, so the
            // profile the user picks lands in the workspace they clicked
            // (a tab always opens in the active workspace). Popover stops the
            // press from reaching the row behind it, so the activation has to
            // run on the capture side of the mouse-down.
            let menu_shell = cx.entity();

            hover_action(
                ("workspace-new-tab", idx),
                t!("sidebar-tab-new"),
                HoverActionLayout::Bare,
                HoverActionVisibility::OnGroupHover("ws-item".into()),
                modern_dropdown(
                    toolbar_button(("workspace-new-tab-button", idx))
                        .icon(IconName::Plus)
                        .accessibility_label(t!("sidebar-tab-new")),
                    move |menu, _, cx| new_tab_menu(menu, &menu_shell, cx),
                ),
            )
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                this.workspaces.list_mut().activate(idx);

                this.on_active_tab_changed(window, cx);

                this.focus_active(window, cx);

                this.sync_session_memory(cx);

                cx.notify();
            }))
            .into_any_element()
        } else if ws.pinned {
            let label = t!("sidebar-workspace-menu-unpin");

            hover_action(
                ("workspace-pin", idx),
                label,
                HoverActionLayout::Inline,
                HoverActionVisibility::OnGroupHover("ws-item".into()),
                Icon::new(PinIcon).small(),
            )
            .into_any_element()
        } else if ws.closeable {
            // Hover-only `×` closes the workspace and drops all of its
            // tabs (panes/PTYs die with the dropped Workspace).
            hover_action(
                ("workspace-close", idx),
                t!("sidebar-workspace-menu-close"),
                HoverActionLayout::Inline,
                HoverActionVisibility::OnGroupHover("ws-item".into()),
                "×",
            )
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
            .children((chrome.agent.unread_count > 0).then(|| {
                div()
                    .id(("workspace-unread", idx))
                    .aria_label(
                        t!(
                            "sidebar-workspace-unread-label",
                            count = chrome.agent.unread_count
                        )
                        .into_owned(),
                    )
                    .size_5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .child(chrome.agent.unread_count.to_string())
            }))
            .children(indicator)
            .child(controls);

        let full_path = ws.cwd.clone();

        // A temporary workspace wears the same `*` an unsaved document does,
        // so its absence from the next session is visible before the user
        // closes the window.
        // Both of these reach the view several times per row and the drag
        // payload once more, so they are built in the form those take rather
        // than copied into it at each use.
        let display_label: SharedString = match ws.temporary {
            true => format!("* {}", workspace_display_label(&ws.name, &ws.cwd)).into(),
            false => workspace_display_label(&ws.name, &ws.cwd).into(),
        };

        // The `+N` token holds a fixed lane beside the path, so the path's own
        // budget shrinks by its width instead of pushing it off the row. The
        // name now shares that line and is charged against the same budget; a
        // name long enough to exhaust it leaves the path at its floor, where
        // the tail still names the leaf directory.
        let additional_count = ws.additional_cwds.len();

        let additional_summary = (additional_count > 0).then(|| {
            t!(
                "sidebar-workspace-additional-count",
                count = additional_count
            )
            .into_owned()
        });

        let path_budget = (self.width
            - 80.0
            - 8.0 * display_label.chars().count() as f32
            - additional_summary
                .as_ref()
                .map_or(0.0, |token| 8.0 + 7.0 * token.chars().count() as f32))
            / 7.0;

        let display_path: SharedString = tail_preserving_path(
            &full_path,
            (path_budget.floor().max(0.0) as usize).clamp(8, 64),
        )
        .into();

        // Tooltip and assistive technology get every directory in order; the
        // row itself only has room for the primary path.
        let dirs_description = workspace_dirs_description(&ws.cwd, &ws.additional_cwds);

        let name = div()
            .id(("workspace-secondary", idx))
            .aria_label(display_label.clone())
            .min_w_0()
            .text_left()
            .text_size(px(WORKSPACE_NAME_TEXT))
            // Only the workspace the user is in takes the heavier weight. With
            // every name at medium the column reads as one solid block, and CJK
            // glyphs carry that weight more heavily than latin ones do.
            .font_weight(if ws.active {
                FontWeight::MEDIUM
            } else {
                FontWeight::NORMAL
            })
            .truncate();

        // Name and path share one line: consecutive rows repeat most of the
        // path prefix, so it earns a trailing lane rather than a line of its
        // own, and the column fits about twice as many workspaces on screen.
        let name: AnyElement = if let Some(input) = renaming {
            let rename_shell = cx.entity();

            InlineRename::new(
                ("workspace-secondary", idx),
                display_label.clone(),
                input,
                InlineRenameStyle::Workspace,
                move |window, cx| {
                    rename_shell.update(cx, |this, cx| {
                        this.finish_workspace_rename(false, window, cx)
                    });
                },
            )
            .into_any_element()
        } else {
            h_flex()
                .w_full()
                .gap_1p5()
                .items_baseline()
                .child(name.child(display_label.clone()))
                // The settings entry has no working directory, so its row
                // carries the name alone.
                .children((!settings_entry).then(|| {
                    div()
                        .id(("workspace-path", idx))
                        .flex_1()
                        .min_w_0()
                        .text_left()
                        .text_size(px(WORKSPACE_PATH_TEXT))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .aria_label(dirs_description.clone())
                        .text_color(cx.theme().sidebar_foreground.opacity(0.4))
                        .child(display_path.clone())
                }))
                .children(additional_summary.map(|token| {
                    div()
                        .id(("workspace-additional-dirs", idx))
                        .flex_none()
                        .text_size(px(WORKSPACE_PATH_TEXT))
                        .aria_label(
                            t!(
                                "sidebar-workspace-additional-label",
                                count = additional_count
                            )
                            .into_owned(),
                        )
                        .text_color(cx.theme().sidebar_foreground.opacity(0.4))
                        .child(token)
                }))
                .into_any_element()
        };

        let drag_name = display_label.clone();
        let drag_cwd = display_path.clone();
        let drag_agent_status = chrome.agent.status;
        let drag_terminal_activity = chrome.terminal_activity;

        // Replicate the item's rendered width: sidebar width minus the card
        // gutter/border and the card's inner paddings around the list.
        let drag_width = (self.width - 36.0).max(80.0);

        let item = workspace_row_button(("workspace", idx), cx)
            .accessibility_label(if settings_entry {
                display_label.clone()
            } else {
                t!(
                    "sidebar-workspace-item-label",
                    name = &display_label,
                    path = &dirs_description,
                    status = &status_label
                )
                .into_owned()
                .into()
            })
            // The active tab's own row is highlighted in the vertical tab-bar
            // style, and it sits under its workspace, so highlighting the
            // workspace too would fill two rows for one selection.
            .selected(highlight_active)
            // Button resolves selected colors after element styles, so the
            // sidebar-accent pair must be the selected custom variant itself.
            .when(highlight_active, |this| {
                this.custom(
                    ButtonCustomVariant::new(cx)
                        .foreground(selection.active_foreground)
                        .active(selection.active_background),
                )
            })
            .group("ws-item")
            .child(
                h_flex()
                    .w_full()
                    .gap_1p5()
                    .items_center()
                    .child(div().flex_1().min_w_0().overflow_hidden().child(name))
                    .child(suffix),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.workspaces.list_mut().activate(idx);

                this.on_active_tab_changed(window, cx);

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

        let pin_label = if pinned {
            t!("sidebar-workspace-menu-unpin")
        } else {
            t!("sidebar-workspace-menu-pin")
        };

        let cwd = ws.cwd.clone();
        let temporary = ws.temporary;

        let progress = (!vertical_tabs)
            .then(|| chrome.progress.fraction())
            .flatten()
            .map(|fraction| workspace_progress_bar(fraction, cx));

        div()
            .id(("workspace-menu", idx))
            .w_full()
            .relative()
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
                    terminal_activity: drag_terminal_activity,
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
            .modern_context_menu(move |menu, _, _| {
                let rename_shell = shell.clone();
                let dirs_shell = shell.clone();
                let close_shell = shell.clone();
                let pin_shell = shell.clone();
                let activate_shell = shell.clone();
                let cwd = cwd.clone();

                // Pinning and closing are the two a user reaches for without
                // reading, so they lead as a row of buttons rather than taking a
                // line each. The settings entry is dismissible and nothing else.
                menu.commands(|row| {
                    row.when(!settings_entry, |row| {
                        row.item(pin_label.clone(), move |_, cx| {
                            pin_shell.update(cx, |this, cx| {
                                this.set_workspace_pinned(ws_id, !pinned, cx)
                            });
                        })
                        .icon(PinIcon)
                    })
                    .item_disabled(
                        t!("sidebar-workspace-menu-close"),
                        !closeable,
                        move |window, cx| {
                            close_shell.update(cx, |this, cx| {
                                this.request_close_workspace(ws_id, window, cx)
                            });
                        },
                    )
                    .icon(IconName::Close)
                })
                // Renaming and copying a path both describe a workspace the user
                // owns, which the settings entry is not.
                .when(!settings_entry, |menu| {
                    menu.item(t!("sidebar-workspace-menu-rename"), move |window, cx| {
                        rename_shell.update(cx, |this, cx| {
                            this.start_workspace_rename(ws_id, window, cx)
                        });
                    })
                    .icon(IconName::PenLine)
                    .item(t!("sidebar-workspace-menu-edit-dirs"), move |window, cx| {
                        dirs_shell
                            .update(cx, |this, cx| this.edit_workspace_dirs(ws_id, window, cx));
                    })
                    .icon(IconName::Folder)
                    .item(t!("sidebar-workspace-menu-copy-path"), move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(cwd.clone()));
                    })
                    .icon(IconName::Copy)
                    // Only a temporary workspace has anything to adopt.
                    .when(temporary, |menu| {
                        menu.item(t!("sidebar-workspace-menu-activate"), move |_, cx| {
                            activate_shell
                                .update(cx, |this, cx| this.activate_as_workspace(ws_id, cx));
                        })
                        .icon(IconName::CircleCheck)
                    })
                })
            })
            .child(item)
            .when(!settings_entry, |row| {
                row.managed_tooltip_right(dirs_description)
            })
            // After the row itself, because the row's selected fill would
            // otherwise paint over the bar's lane.
            .children(highlight_active.then(|| selection_bar(cx)))
            .children(progress)
            .into_any_element()
    }

    /// One tab of a workspace, rendered as a child row under it. Clicking it
    /// switches to that workspace *and* that tab, so a row under an inactive
    /// workspace is a single-click jump rather than a two-step one.
    fn render_tab_row(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        tab: &SidebarTab,
        closeable: bool,
        renames: &InlineRenameSession,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        let tab_id = tab.id;
        let key = tab_id.0 as usize;
        let active = tab.active;
        let selection = sidebar_selection(cx);

        let close = hover_action(
            ("sidebar-tab-close", key),
            t!("tabbar-menu-close"),
            HoverActionLayout::Inline,
            HoverActionVisibility::OnGroupHover("sidebar-tab".into()),
            "\u{00d7}",
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();

            this.request_close_tab(tab_id, window, cx);
        }));

        // One mark per row, because the lane it sits in is one glyph wide.
        // Ordered by what is worth acting on first: an agent mid-turn, then
        // what the shell is doing, then output nobody has read. A tab's own
        // kind of status therefore outranks the generic unread dot, the way
        // the horizontal strip orders them too. A row with nothing to report
        // shows no mark: in a list where most rows are quiet, a placeholder on
        // each of them is what the eye has to filter out to find the busy one.
        let status_mark: Option<AnyElement> = match (tab.agent_kind.is_some(), tab.busy) {
            (true, true) => Some(
                StatusMark::new(
                    ("sidebar-tab-busy", key),
                    StatusMarkTone::Warning,
                    px(TAB_ROW_DOT),
                )
                .pulse()
                .label(t!("sidebar-workspace-status-running"))
                .into_any_element(),
            ),
            _ => terminal_presentation(tab.terminal)
                .map(|(visual, aria)| {
                    div()
                        .id(("sidebar-tab-terminal", key))
                        .aria_label(aria)
                        .flex()
                        .child(terminal_dot(visual, TAB_ROW_DOT, cx))
                        .into_any_element()
                })
                .or_else(|| {
                    // Same success color the horizontal strip gives an
                    // unread notification, so one meaning keeps one color
                    // across both tab styles.
                    tab.unread.then(|| {
                        StatusMark::new(
                            ("sidebar-tab-unread", key),
                            StatusMarkTone::Success,
                            px(TAB_ROW_DOT),
                        )
                        .label(t!("sidebar-workspace-unread-label", count = "1").into_owned())
                        .into_any_element()
                    })
                }),
        };

        let renaming = renames.tab_input(tab_id).cloned();

        let label: AnyElement = match renaming {
            Some(input) => {
                let rename_shell = cx.entity();

                InlineRename::new(
                    ("sidebar-tab-rename", key),
                    tab.label.clone(),
                    input,
                    InlineRenameStyle::SidebarTab,
                    move |window, cx| {
                        rename_shell
                            .update(cx, |this, cx| this.finish_tab_rename(false, window, cx));
                    },
                )
                .into_any_element()
            }
            None => div()
                .flex_1()
                .overflow_hidden()
                .truncate()
                .when(tab.exited, |this| this.text_color(cx.theme().danger))
                .child(tab.label.clone())
                .into_any_element(),
        };

        let menu_shell = cx.entity();
        let drag_shell = cx.entity();
        let drag_label = tab.label.clone();

        // The row spans the list column: sidebar width minus the panel inset
        // on both sides and the scrollbar lane the list reserves.
        let drag_width = (self.width - SIDEBAR_PADDING_X * 2.0 - 12.0).max(80.0);

        let row = h_flex()
            .id(("sidebar-tab", key))
            // The row is the selectable thing in this style: its fill is the
            // only cue that a tab is the one on screen, so it carries the
            // selected state assistive technology reads. The label is stated
            // rather than derived, because the row also holds status marks and
            // swaps its text for an input while the tab is being renamed.
            .role(Role::Tab)
            .aria_label(tab.label.clone())
            .aria_selected(active)
            .group("sidebar-tab")
            .relative()
            .w_full()
            .h(px(TAB_ROW_HEIGHT))
            // The same inset the workspace rows take, so the glyph column
            // stands on the same edge as the workspace names above it. A tab
            // is tied to its workspace by the gap that separates one such
            // block from the next rather than by an indent.
            .px(px(SIDEBAR_ROW_GUTTER))
            .gap(px(TAB_ROW_GAP))
            .items_center()
            .rounded(UI_RADIUS)
            .text_size(px(TAB_ROW_TEXT))
            // A restored-but-not-yet-spawned tab renders faded, the same
            // "sleeping tab" cue the horizontal strip uses.
            .when(tab.pending, |this| this.opacity(0.6))
            // The selected row is marked by its fill alone. The fill
            // already separates the row from the list at a glance; adding an
            // outline and a heavier weight on top of it states the same thing
            // three times, and the weight change also reflows the label.
            .when(active, |this| {
                this.bg(selection.active_background)
                    .text_color(selection.active_foreground)
            })
            .when(!active, |this| {
                this.text_color(selection.idle_foreground)
                    .hover(|this| this.bg(selection.hover_background))
            })
            // The status mark takes over the type icon's slot instead of
            // claiming a lane of its own ahead of it. What a session is doing
            // is what the eye scans the list for, and its kind only matters
            // once the row is found; sharing the slot also keeps the label
            // from shifting sideways the moment a tab starts working.
            .child(
                div()
                    .flex_none()
                    .size(px(TAB_ROW_ICON))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(match (status_mark, tab.pending) {
                        (Some(mark), _) => mark,
                        (None, true) => {
                            pending_tab_icon(("sidebar-tab-pending", key)).into_any_element()
                        }
                        (None, false) => tab.icon.clone().into_any_element(),
                    }),
            )
            .child(label)
            // Bell dot, in the warning color so it reads apart from the unread
            // dot when a tab carries both.
            .children(tab.bell.then(|| {
                StatusMark::new(
                    ("sidebar-tab-bell", key),
                    StatusMarkTone::Warning,
                    px(TAB_ROW_DOT),
                )
            }))
            .when(closeable, |this| this.child(close))
            .children(tab.progress.map(|report| {
                let (color, fraction) = progress_visual(report, cx);

                div()
                    .absolute()
                    .bottom_0()
                    .left(UI_RADIUS)
                    .right(UI_RADIUS)
                    .h(px(2.0))
                    .child(
                        div()
                            .h_full()
                            .w(relative(fraction))
                            .rounded_full()
                            .bg(color),
                    )
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.workspaces.list_mut().activate(ws_idx);

                this.workspaces
                    .active_tabs_mut()
                    .list_mut()
                    .activate(tab_idx);

                this.on_active_tab_changed(window, cx);

                this.focus_active(window, cx);

                this.sync_session_memory(cx);

                cx.notify();
            }));

        div()
            .id(("sidebar-tab-menu", key))
            .w_full()
            .when(self.tab_dragging == Some((ws_idx, tab_idx)), |this| {
                this.opacity(0.0)
            })
            // Make way for the dragged row: the hovered row slides down,
            // opening an insertion gap at the pointer.
            .when(self.tab_drag_over == Some((ws_idx, tab_idx)), |this| {
                this.mt(px(TAB_ROW_HEIGHT))
            })
            .on_drag(
                SidebarTabDrag {
                    workspace: ws_idx,
                    from: tab_idx,
                    tab: tab_id,
                },
                move |_, _, _, cx| {
                    drag_shell.update(cx, |this, cx| {
                        this.sidebar.tab_dragging = Some((ws_idx, tab_idx));

                        cx.notify();
                    });

                    cx.new(|_| DragLabelPreview {
                        style: DragStyle::Sidebar,
                        label: drag_label.clone(),
                        width: drag_width,
                    })
                },
            )
            .on_drag_move(
                cx.listener(move |this, e: &DragMoveEvent<SidebarTabDrag>, _, cx| {
                    if !e.bounds.contains(&e.event.position) {
                        return;
                    }

                    let drag = e.drag(cx);

                    // No gap over the drag's own row, and none over another
                    // workspace's rows, where the drop would be refused.
                    let target = (drag.workspace == ws_idx && drag.from != tab_idx)
                        .then_some((ws_idx, tab_idx));

                    if this.sidebar.tab_drag_over != target {
                        this.sidebar.tab_drag_over = target;

                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &SidebarTabDrag, window, cx| {
                // The list-level fallback handler must not also reorder this
                // drop.
                cx.stop_propagation();

                this.sidebar.tab_drag_over = None;
                this.sidebar.tab_dragging = None;

                if drag.workspace == ws_idx {
                    this.reorder_tab(drag.tab, drag.from, tab_idx, window, cx);
                }

                cx.notify();
            }))
            .modern_context_menu(move |menu, _, _| {
                let rename_shell = menu_shell.clone();
                let close_shell = menu_shell.clone();

                menu.item(t!("tabbar-menu-rename"), move |window, cx| {
                    rename_shell.update(cx, |this, cx| this.start_tab_rename(tab_id, window, cx));
                })
                .icon(IconName::PenLine)
                .item_disabled(t!("tabbar-menu-close"), !closeable, move |window, cx| {
                    close_shell.update(cx, |this, cx| this.request_close_tab(tab_id, window, cx));
                })
                .icon(IconName::Close)
            })
            .child(row)
            .into_any_element()
    }
}

/// The agent half of the status column, absent while the agent is idle.
fn agent_presentation(status: AgentRuntimeStatus) -> Option<(AgentVisual, Cow<'static, str>)> {
    match status {
        AgentRuntimeStatus::Running => {
            Some((AgentVisual::Running, t!("sidebar-workspace-status-running")))
        }
        AgentRuntimeStatus::NeedsInput => Some((
            AgentVisual::NeedsInput,
            t!("sidebar-workspace-status-needs-input"),
        )),
        AgentRuntimeStatus::Idle => None,
    }
}

/// One accessible label for whatever the column holds. The two halves report
/// independent things, so both are named when both are showing.
fn status_column_label(agent: Option<&str>, terminal: Option<&str>) -> String {
    match (agent, terminal) {
        (Some(agent), Some(terminal)) => t!(
            "sidebar-workspace-status-pair",
            agent = agent,
            terminal = terminal
        )
        .into_owned(),
        (Some(label), None) | (None, Some(label)) => label.to_string(),
        (None, None) => t!("sidebar-workspace-status-idle").to_string(),
    }
}

/// Glyphs for the status column, agent above terminal. The caller stacks them;
/// with one glyph the stack collapses to a centered single mark.
fn workspace_status_glyphs(
    status: AgentRuntimeStatus,
    terminal: TerminalActivity,
    busy_id: impl Into<ElementId>,
    cx: &gpui::App,
) -> (Vec<AnyElement>, String) {
    let agent = agent_presentation(status);
    let terminal = terminal_presentation(terminal);

    let label = status_column_label(
        agent.as_ref().map(|(_, label)| label.as_ref()),
        terminal.as_ref().map(|(_, label)| label.as_ref()),
    );

    let busy_id = busy_id.into();

    let glyphs = agent
        .map(|(visual, label)| match visual {
            AgentVisual::Running => StatusMark::busy(busy_id).into_any_element(),
            // Same success color the terminal mark uses when a command
            // finishes: both say the tab has stopped working and is waiting on
            // the user.
            AgentVisual::NeedsInput => {
                StatusMark::new(busy_id, StatusMarkTone::Success, px(STATUS_DOT))
                    .label(label)
                    .into_any_element()
            }
        })
        .into_iter()
        .chain(terminal.map(|(visual, _)| terminal_dot(visual, STATUS_DOT, cx)))
        .collect();

    (glyphs, label)
}

/// Progress bar along the bottom edge of a sidebar item, driven by the combined
/// OSC 9;4 progress of the workspace's tabs. One corner radius of space at each
/// side keeps the track on the straight part of the bottom edge.
fn workspace_progress_bar(fraction: f32, cx: &gpui::App) -> AnyElement {
    div()
        .absolute()
        .bottom_0()
        .left(UI_RADIUS)
        .right(UI_RADIUS)
        .h(px(2.0))
        .child(
            div()
                .h_full()
                .w(relative(fraction))
                .rounded_full()
                .bg(cx.theme().primary),
        )
        .into_any_element()
}

fn workspace_display_label(name: &str, cwd: &str) -> String {
    if name != "New Workspace" && name != t!("shell-workspace-default-name") {
        return name.to_string();
    }

    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .filter(|component| *component != ".")
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string())
}

/// The full ordered directory list a workspace row exposes through its tooltip
/// and accessibility text: the primary path first, marked as primary, then
/// every additional path in workspace order.
fn workspace_dirs_description(cwd: &str, additional: &[String]) -> String {
    let mut description = t!("sidebar-workspace-primary-label", path = cwd).into_owned();

    for path in additional {
        description.push('\n');

        description.push_str(path);
    }

    description
}

fn tail_preserving_path(path: &str, max_chars: usize) -> String {
    let length = path.chars().count();

    if length <= max_chars || max_chars == 0 {
        return path.to_string();
    }

    if max_chars == 1 {
        return "…".to_string();
    }

    let raw_tail = path
        .chars()
        .skip(length - (max_chars - 1))
        .collect::<String>();

    let component_tail = raw_tail
        .find(['/', '\\'])
        .map(|separator| &raw_tail[separator..])
        .filter(|tail| tail.len() > 1)
        .unwrap_or(&raw_tail);

    format!("…{component_tail}")
}

/// Sidebar pinned-workspace glyph (`assets/icons/pin.svg`).
struct PinIcon;

impl IconNamed for PinIcon {
    fn path(self) -> SharedString {
        "icons/pin.svg".into()
    }
}

/// Terminal with a lower-right close mark
/// (`assets/icons/terminal-close.svg`).
struct CloseTemporaryWorkspacesIcon;

impl IconNamed for CloseTemporaryWorkspacesIcon {
    fn path(self) -> SharedString {
        "icons/terminal-close.svg".into()
    }
}

/// One tab rendered as a child row of its workspace, in the vertical tab-bar
/// style. Snapshotted out of the tab manager before the render closures borrow
/// the shell.
pub(crate) struct SidebarTab {
    pub(crate) id: TabId,
    pub(crate) label: SharedString,
    pub(crate) active: bool,
    pub(crate) unread: bool,
    pub(crate) busy: bool,
    pub(crate) bell: bool,
    pub(crate) agent_kind: Option<AgentKind>,
    pub(crate) icon: Icon,

    /// Restored but not yet spawned.
    pub(crate) pending: bool,

    pub(crate) exited: bool,
    pub(crate) progress: Option<ProgressReport>,
    pub(crate) terminal: TerminalActivity,
}

/// Diameter of a tab row's status dot. Smaller than the workspace column's,
/// which keeps the two tiers apart at a glance.
const TAB_ROW_DOT: f32 = 7.0;

/// Spacing inside a tab row, between its glyph and its label.
const TAB_ROW_GAP: f32 = 6.0;

/// Edge of a tab row's type icon, and the size its label is set at.
const TAB_ROW_ICON: f32 = 14.0;

const TAB_ROW_TEXT: f32 = 13.0;

/// Insets and rhythm of the workspace list. Groups are spaced further apart
/// than the rows inside them, which is what makes a workspace and its tabs
/// read as one block rather than as a flat list.
///
/// The panel owns the horizontal text inset; row fills extend into its gutter.
const SIDEBAR_PADDING_X: f32 = 12.0;

/// How far a row's fill reaches back into that inset on each side, and how
/// much padding the row then puts back so its content still lands on the
/// column's edge. Without it the highlight stops exactly where the first
/// glyph starts and reads as clipped; the leading half of it is also the lane
/// the selected-row mark stands in.
const SIDEBAR_ROW_GUTTER: f32 = 6.0;

// Remove the outer panel offset and restore the row's negative margin so
// the visible row fill starts directly below the native close button.
#[cfg(target_os = "macos")]
const SIDEBAR_PADDING_LEFT: f32 =
    TRAFFIC_LIGHT_INSET - FLOATING_SURFACE_SIDE_INSET + SIDEBAR_ROW_GUTTER;

#[cfg(not(target_os = "macos"))]
const SIDEBAR_PADDING_LEFT: f32 = SIDEBAR_PADDING_X;

const SIDEBAR_GROUP_GAP: f32 = 8.0;
const WORKSPACE_LIST_GAP: f32 = 6.0;

/// The heading uses one size across languages to keep the section easy to scan.
const SIDEBAR_SECTION_TEXT: f32 = 12.0;

/// A workspace heading: its name, and the path that trails it on the same
/// line. The path is set small enough to read as an annotation on the name.
const WORKSPACE_NAME_TEXT: f32 = 13.0;

const WORKSPACE_PATH_TEXT: f32 = 10.5;

/// The status cluster along the bottom edge: today's spend over the
/// subscription gauges. Both report what the agents have consumed, so they
/// stack as one block under a single rule rather than each carrying an edge.
const SIDEBAR_STATUS_PADDING_TOP: f32 = 8.0;

const SIDEBAR_STATUS_PADDING_BOTTOM: f32 = 2.0;
const SIDEBAR_STATUS_ROW_GAP: f32 = 2.0;

/// Distance from the row box's leading edge. The row is a rounded rectangle,
/// so a mark flush against that edge would sit outside the fill at the corners.
const SELECTION_BAR_INSET: f32 = 2.0;

/// Names retain an 8px gap after the selection mark on active and idle rows.
const WORKSPACE_NAME_INSET: f32 = SELECTION_BAR_INSET + SELECTION_BAR_WIDTH + 8.0;

/// The accent bar that marks the selected row. It is drawn out of the row's
/// flow so it can sit in the gutter left of the row's own padding, and it
/// carries the accent color on its own: the row fill stays a neutral subtle
/// wash, which keeps a selected row legible against a translucent pane.
fn selection_bar(cx: &App) -> impl IntoElement {
    div()
        .absolute()
        .left(px(SELECTION_BAR_INSET))
        .top_0()
        .bottom_0()
        .flex()
        .items_center()
        .child(
            div()
                .w(px(SELECTION_BAR_WIDTH))
                .h(px(SELECTION_BAR_HEIGHT))
                .rounded(px(SELECTION_BAR_RADIUS))
                .bg(cx.theme().primary),
        )
}

/// The two readouts the status cluster draws. The shell owns both, so their
/// refresh loops survive a sidebar collapse and a tab-bar style change.
pub(crate) struct SidebarUsage {
    pub(crate) daily: Entity<TokenUsageView>,
    pub(crate) quotas: Entity<AgentUsageView>,
}

/// How far a workspace item slides down to open the insertion gap while a
/// drag hovers it.
const WS_MAKE_WAY_PX: f32 = 36.0;

fn workspace_list_scrollbar(handle: &ScrollHandle) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(16.0))
        .child(Scrollbar::vertical(handle))
}

pub(super) fn workspace_row_button(id: impl Into<ElementId>, cx: &App) -> Button {
    let selection = sidebar_selection(cx);

    Button::new(id)
        // Button registers its own hover handler; variants supply its colors
        // without installing a second hover style on the same element.
        .custom(
            ButtonCustomVariant::new(cx)
                .color(cx.theme().sidebar_foreground.opacity(0.045))
                .hover(cx.theme().sidebar_foreground.opacity(0.085))
                .active(selection.active_background),
        )
        .w_full()
        .h_auto()
        // Base buttons use a one-em line box; clipped directory text needs
        // leading so descenders remain visible inside the row.
        .line_height(relative(1.5))
        // Reserve the mark's width and a readable gap without moving the
        // row background or changing its trailing alignment.
        .pl(px(WORKSPACE_NAME_INSET))
        .pr(px(SIDEBAR_ROW_GUTTER))
        .py_0p5()
}
