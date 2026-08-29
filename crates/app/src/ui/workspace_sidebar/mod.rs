use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DragMoveEvent, ElementId, Entity, FontWeight, ScrollHandle,
    SharedString, div, px, relative,
};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::input::InputState;
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::scroll::Scrollbar;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IconNamed, Selectable, Sizable, h_flex, v_flex,
};
use nmt_agent_utils::AgentRuntimeStatus;
use nmt_app_agent::AgentKind;
use nmt_config::appearance::TabBarStyle;
use nmt_i18n::i18n;
use nmt_terminal::event::ProgressReport;

use crate::agent_usage::AgentUsageView;
use crate::tabs::TabId;
use crate::ui::composition::{
    FLOATING_SURFACE_BOTTOM_INSET, FLOATING_SURFACE_SIDE_INSET, FLOATING_SURFACE_TOP_INSET,
    HoverActionLayout, HoverActionVisibility, StatusMark, StatusMarkTone, hover_action,
    sidebar_selection,
};
use crate::ui::fluent::{SELECTION_BAR_HEIGHT, SELECTION_BAR_RADIUS, SELECTION_BAR_WIDTH};
use crate::ui::shell::{InlineRename, InlineRenameStyle, pending_tab_icon};
use crate::ui::sidebar_resize::{self, ResizeDrag};
use crate::ui::tab_bar::{new_tab_menu, progress_visual, tab_icon};
use crate::ui::terminal_status::{terminal_dot, terminal_presentation};
use crate::ui::token_usage::TokenUsageView;
use crate::ui::{AppSettings, NewWorkspace, Shell, UI_RADIUS};
use crate::window::WindowRegistry;
use crate::workspace::{TerminalActivity, WorkspaceId, WorkspaceKind, WorkspaceSummary};

mod drag;
mod rows;
#[cfg(test)]
mod tests;

use crate::ui::workspace_sidebar::drag::{
    SidebarTabDrag, SidebarTabDragPreview, WorkspaceDrag, WorkspaceDragPreview,
};

/// Default expanded width of the workspace sidebar, in pixels; the user can
/// drag the right edge to resize.
pub(super) const SIDEBAR_WIDTH: f32 = 180.0;
/// Drag limits: keep the workspace list readable and leave room for the terminal.
/// Handle id for this column's resize grip. Every resizable column receives
/// every other column's drag-move events, so this is what distinguishes them.
pub(super) const RESIZE_HANDLE: &str = "workspace-sidebar-resize";

pub(super) const MIN_WIDTH: f32 = 140.0;
pub(crate) const MAX_WIDTH: f32 = 480.0;

/// Side of the vertical tab-bar new-tab control on a workspace row, and the
/// size the `+` glyph inside it is drawn at. The control is hover-only, so it
/// is sized as a comfortable pointer target rather than to match the `×` it
/// replaces.
const NEW_TAB_BUTTON: f32 = 32.0;
const NEW_TAB_GLYPH: f32 = 18.0;

/// Diameter of a status dot in the sidebar column. Smaller than the agent
/// spinner's `size_3`, so a stacked pair reads as a spinner with a mark under
/// it rather than as two equal glyphs.
const STATUS_DOT: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentVisual {
    Running,
    NeedsInput,
}

/// The agent half of the status column, absent while the agent is idle.
fn agent_presentation(status: AgentRuntimeStatus) -> Option<(AgentVisual, &'static str)> {
    match status {
        AgentRuntimeStatus::Running => Some((
            AgentVisual::Running,
            i18n("sidebar-workspace-status-running"),
        )),
        AgentRuntimeStatus::NeedsInput => Some((
            AgentVisual::NeedsInput,
            i18n("sidebar-workspace-status-needs-input"),
        )),
        AgentRuntimeStatus::Idle => None,
    }
}

/// One accessible label for whatever the column holds. The two halves report
/// independent things, so both are named when both are showing.
fn status_column_label(agent: Option<&'static str>, terminal: Option<&'static str>) -> String {
    match (agent, terminal) {
        (Some(agent), Some(terminal)) => i18n("sidebar-workspace-status-pair")
            .replace("{agent}", agent)
            .replace("{terminal}", terminal),
        (Some(label), None) | (None, Some(label)) => label.to_string(),
        (None, None) => i18n("sidebar-workspace-status-idle").to_string(),
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
        agent.map(|(_, label)| label),
        terminal.map(|(_, label)| label),
    );

    let busy_id = busy_id.into();

    let glyphs = agent
        .map(|(visual, label)| match visual {
            AgentVisual::Running => StatusMark::busy(busy_id).into_any_element(),
            AgentVisual::NeedsInput => {
                StatusMark::new(busy_id, StatusMarkTone::Primary, px(STATUS_DOT))
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
    if name != "New Workspace" && name != i18n("shell-workspace-default-name") {
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
    let mut description = i18n("sidebar-workspace-primary-label").replace("{path}", cwd);
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
    pub(crate) settings: bool,
    /// Restored but not yet spawned.
    pub(crate) pending: bool,
    pub(crate) exited: bool,
    pub(crate) progress: Option<ProgressReport>,
    pub(crate) terminal: TerminalActivity,
}

/// Height of a tab row. A workspace item stacks a name and a path line, so a
/// row stays visibly shorter than one and the two tiers read as ranked, while
/// leaving the row a comfortable click target.
const TAB_ROW_HEIGHT: f32 = 28.0;

/// Diameter of a tab row's status dot. Smaller than the workspace column's,
/// which keeps the two tiers apart at a glance.
const TAB_ROW_DOT: f32 = 7.0;

/// How far a tab row is indented under the workspace it belongs to, and the
/// spacing inside the row itself.
const TAB_ROW_INDENT: f32 = 6.0;
const TAB_ROW_PADDING_X: f32 = 8.0;
const TAB_ROW_GAP: f32 = 8.0;
/// Edge of a tab row's type icon, and the size its label is set at.
const TAB_ROW_ICON: f32 = 14.0;
const TAB_ROW_TEXT: f32 = 13.0;

/// Insets and rhythm of the workspace list. Groups are spaced further apart
/// than the rows inside them, which is what makes a workspace and its tabs
/// read as one block rather than as a flat list.
const SIDEBAR_PADDING_X: f32 = 12.0;
const SIDEBAR_PADDING_TOP: f32 = 14.0;
const SIDEBAR_GROUP_GAP: f32 = 10.0;
/// The list heading. It names the column rather than competing with the
/// workspaces under it, so it is the smallest run of text in the panel.
const SIDEBAR_SECTION_TEXT: f32 = 11.0;
const SIDEBAR_SECTION_BUTTON: f32 = 20.0;
/// A workspace heading: its name, and the path line under it.
const WORKSPACE_NAME_TEXT: f32 = 13.0;
const WORKSPACE_PATH_TEXT: f32 = 11.0;
/// The status cluster along the bottom edge: today's spend over the
/// subscription gauges. Both report what the agents have consumed, so they
/// stack as one block under a single rule rather than each carrying an edge.
const SIDEBAR_STATUS_PADDING_Y: f32 = 8.0;
const SIDEBAR_STATUS_ROW_GAP: f32 = 2.0;

/// Distance from the row box's leading edge. The row is a rounded rectangle,
/// so a mark flush against that edge would sit outside the fill at the corners.
const SELECTION_BAR_INSET: f32 = 2.0;

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
            tab_drag_over: None,
            tab_dragging: None,
        }
    }

    /// The workspace sidebar: one themed button per workspace (active = selected),
    /// plus a new-workspace button and bottom status bar. Toggled by
    /// `ToggleSidebar` (Ctrl+Shift+B).
    pub(super) fn render(
        &mut self,
        summaries: Vec<WorkspaceSummary>,
        // One entry per summary in the vertical tab-bar style, empty in the
        // horizontal one where the title bar still owns the tabs.
        tabs: Vec<Vec<SidebarTab>>,
        rename: Option<&(WorkspaceId, Entity<InputState>)>,
        tab_rename: Option<&(TabId, Entity<InputState>)>,
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
        let has_temporary_workspaces = summaries.iter().any(|workspace| workspace.temporary);
        let show_daily_usage = cx.global::<AppSettings>().show_daily_token_usage;
        let show_quotas = cx.global::<AppSettings>().show_agent_usage;

        // Fixed-width content; the animated wrapper below clips it so the buttons
        // don't reflow while the sidebar slides. The transparent panel inherits
        // the window background while the drag and animation math keeps operating
        // on the full `width`.
        let panel = div()
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .px(px(SIDEBAR_PADDING_X))
            .pt(px(SIDEBAR_PADDING_TOP))
            .gap(px(SIDEBAR_GROUP_GAP))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(SIDEBAR_SECTION_TEXT))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().sidebar_foreground.opacity(0.65))
                            .child(i18n("sidebar-workspaces-title")),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("new-workspace")
                                    .ghost()
                                    .size(px(SIDEBAR_SECTION_BUTTON))
                                    .icon(IconName::Plus)
                                    .aria_label(i18n("shell-workspace-new-title"))
                                    .tooltip(i18n("shell-workspace-new-title"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.on_new_workspace(&NewWorkspace, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("close-temporary-workspaces")
                                    .ghost()
                                    .size(px(SIDEBAR_SECTION_BUTTON))
                                    .icon(CloseTemporaryWorkspacesIcon)
                                    .aria_label(i18n("sidebar-workspace-close-temporary"))
                                    .tooltip(i18n("sidebar-workspace-close-temporary"))
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
                    .child(
                        v_flex()
                            .id("workspace-list")
                            .size_full()
                            .gap(px(SIDEBAR_GROUP_GAP))
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
                                rows.push(self.render_item(idx, ws, rename, cx));
                                let ws_tabs = tabs.get(idx).map(Vec::as_slice).unwrap_or_default();
                                // Closing a workspace's last tab falls through to
                                // closing the workspace, so the row keeps its
                                // control as long as one of the two would take
                                // effect. A pinned or sole workspace refuses both,
                                // and the row withholds a control that would do
                                // nothing.
                                let closeable = ws_tabs.len() > 1 || ws.closeable;

                                rows.extend(ws_tabs.iter().enumerate().map(|(tab_idx, tab)| {
                                    self.render_tab_row(
                                        idx, tab_idx, tab, closeable, tab_rename, cx,
                                    )
                                }));

                                v_flex().w_full().children(rows)
                            })),
                    )
                    .child(workspace_list_scrollbar(&self.scroll)),
            )
            .children((show_daily_usage || show_quotas).then(|| {
                v_flex()
                    .id("workspace-sidebar-status")
                    .w_full()
                    .flex_none()
                    .gap(px(SIDEBAR_STATUS_ROW_GAP))
                    .py(px(SIDEBAR_STATUS_PADDING_Y))
                    .border_t_1()
                    .border_color(cx.theme().sidebar_border)
                    .children(show_daily_usage.then_some(usage.daily))
                    .children(show_quotas.then_some(usage.quotas))
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
}

fn workspace_list_scrollbar(handle: &ScrollHandle) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(16.0))
        .child(Scrollbar::vertical(handle))
}
