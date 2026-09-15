mod drag;
mod list;
mod status;

use gpui::prelude::*;
use gpui::{AnyElement, Context, DragMoveEvent, Entity, FontWeight, SharedString, div, px};
use gpui_component::{ActiveTheme, Disableable, IconName, IconNamed, h_flex, v_flex};
use nmt_agent::AgentProjection;
use rust_i18n::t;

use crate::agent_usage::AgentUsageView;
use crate::ui::composition::{
    FLOATING_SURFACE_BOTTOM_INSET, FLOATING_SURFACE_SIDE_INSET, FLOATING_SURFACE_TOP_INSET,
    toolbar_button,
};
use crate::ui::fluent::SELECTION_BAR_WIDTH;
use crate::ui::platform_style::{Host, PlatformStyle as _};
use crate::ui::shell::InlineRenameSession;
use crate::ui::sidebar_resize::ResizeDrag;
use crate::ui::title_bar::TITLE_BAR_CONTROLS_WIDTH;
use crate::ui::token_usage::TokenUsageView;
use crate::ui::workspace_sidebar::list::WorkspaceList;
use crate::ui::{AppSettings, NewWorkspace, Shell, sidebar_resize};
use crate::window::WindowRegistry;
use crate::workspace::{ProgressTally, TerminalActivity, WorkspaceSummary};

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

/// The sidebar never drops below the width its workspace rows need, and
/// never below the title bar's leading control row, which starts past the
/// host's leading inset and so needs more room where the window buttons come
/// first.
pub(super) const MIN_WIDTH: f32 = f32::max(
    140.0,
    Host::TITLE_BAR_LEADING_INSET + TITLE_BAR_CONTROLS_WIDTH - FLOATING_SURFACE_SIDE_INSET,
);

pub(crate) const MAX_WIDTH: f32 = 480.0;

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

    list: WorkspaceList,
}

impl Sidebar {
    pub(super) fn new(width: f32) -> Self {
        Self {
            collapsed: false,
            animated: false,
            width,
            list: WorkspaceList::new(),
        }
    }

    /// Width of a tab row in the vertical tab-bar style: the sidebar width
    /// minus the panel inset on both sides and the scrollbar lane the list
    /// reserves.
    pub(super) fn tab_row_width(&self) -> f32 {
        (self.width - SIDEBAR_PADDING_X * 2.0 - 12.0).max(80.0)
    }

    /// The workspace sidebar: one themed button per workspace (active = selected),
    /// plus a new-workspace button and bottom status bar. Toggled by
    /// `ToggleSidebar` (Ctrl+Shift+B).
    pub(super) fn render(
        &mut self,
        summaries: Vec<WorkspaceChrome>,
        // One entry of tab rows per summary in the vertical tab-bar style,
        // empty in the horizontal one where the title bar still owns the tabs.
        tab_rows: Vec<Vec<AnyElement>>,
        renames: &InlineRenameSession,
        usage: SidebarUsage,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        self.list.end_cancelled_drag(cx);

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
            .child(self.list.render(&summaries, tab_rows, renames, width, cx))
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
                            .then(|| Host::sidebar_agent_usage(usage.quotas.into_any_element())),
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
}

/// Terminal with a lower-right close mark
/// (`assets/icons/terminal-close.svg`).
struct CloseTemporaryWorkspacesIcon;

impl IconNamed for CloseTemporaryWorkspacesIcon {
    fn path(self) -> SharedString {
        "icons/terminal-close.svg".into()
    }
}

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
pub(super) const SIDEBAR_ROW_GUTTER: f32 = 6.0;

/// Where the host draws its window buttons over the title bar, the visible
/// row fill starts directly below the close button: the outer panel offset
/// comes off and the row's negative margin is restored. Elsewhere the panel's
/// own inset applies.
const SIDEBAR_PADDING_LEFT: f32 = match Host::WINDOW_CONTROLS_INSET {
    Some(inset) => inset - FLOATING_SURFACE_SIDE_INSET + SIDEBAR_ROW_GUTTER,
    None => SIDEBAR_PADDING_X,
};

const SIDEBAR_GROUP_GAP: f32 = 8.0;

/// The heading uses one size across languages to keep the section easy to scan.
const SIDEBAR_SECTION_TEXT: f32 = 12.0;

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

/// The two readouts the status cluster draws. The shell owns both, so their
/// refresh loops survive a sidebar collapse and a tab-bar style change.
pub(crate) struct SidebarUsage {
    pub(crate) daily: Entity<TokenUsageView>,
    pub(crate) quotas: Entity<AgentUsageView>,
}
