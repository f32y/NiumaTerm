pub(super) use app::design::SIDEBAR_ROW_GUTTER;

mod drag;
mod list;
mod remote_list;
mod status;

use std::collections::{HashMap, HashSet};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Div, DragMoveEvent, Entity, FontWeight, SharedString,
    StyleRefinement, Window, div, px,
};
use gpui_component::{ActiveTheme, Disableable, IconName, IconNamed, h_flex, v_flex};
use nmt_agent::AgentProjection;
use nmt_config::local_state::TabFold;
use rust_i18n::t;

use crate::agent_usage::AgentUsageView;
use crate::ui::composition::{FLOATING_SURFACE_BOTTOM_INSET, TOOLBAR_ICON_SIZE, toolbar_button};
use crate::ui::fluent::SELECTION_BAR_WIDTH;
use crate::ui::platform_style::{Host, PlatformStyle as _};
use crate::ui::remote::RemoteWorkspace;
use crate::ui::shell::InlineRenameSession;
use crate::ui::sidebar_resize::ResizeDrag;
use crate::ui::title_bar::TITLE_BAR_CONTROLS_WIDTH;
use crate::ui::token_usage::{STATUS_ROW_HEIGHT, TokenUsageView};
use crate::ui::workspace_sidebar::list::WorkspaceList;
use crate::ui::workspace_sidebar::remote_list::remote_workspace_blocks;
use crate::ui::{AppSettings, AppWindow, NewWorkspace, WindowRegistry, sidebar_resize};
use crate::workspace::{ProgressTally, TerminalActivity, WorkspaceSummary};

pub(super) struct WorkspaceChrome {
    /// Position of the workspace in the window's whole list, which also
    /// holds the remote entries this list leaves out.
    pub index: usize,

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
    Host::TITLE_BAR_LEADING_INSET + TITLE_BAR_CONTROLS_WIDTH,
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

    /// How many of its sessions each host workspace lists, by host id and the
    /// workspace's id (its label on hosts that send none). Unlike a local
    /// fold this lasts for the run only: a host's workspace ids are its own
    /// runtime ids, which name other workspaces once it restarts. A workspace
    /// missing here lists every session.
    remote_folds: HashMap<(String, String), TabFold>,

    /// The hosts whose rows hide everything listed under them, by host id.
    /// Hiding a host leaves its workspaces' own folds as they are, so
    /// showing it again brings each workspace back the way it was. Kept for
    /// the run only like the workspace folds, so every host shows whenever
    /// this computer starts.
    remote_collapsed_hosts: HashSet<String>,
}

impl Sidebar {
    pub(super) fn new(width: f32) -> Self {
        Self {
            collapsed: false,
            animated: false,
            width,
            list: WorkspaceList::new(),
            remote_folds: HashMap::default(),
            remote_collapsed_hosts: HashSet::default(),
        }
    }

    /// Step a host workspace's session list on to its next fold.
    pub(super) fn cycle_remote_fold(&mut self, host: String, workspace: String) {
        let fold = self.remote_folds.entry((host, workspace)).or_default();

        *fold = fold.next();
    }

    /// Show or hide everything a host lists under its row.
    pub(super) fn toggle_remote_host(&mut self, host: String) {
        if !self.remote_collapsed_hosts.remove(&host) {
            self.remote_collapsed_hosts.insert(host);
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
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render(
        &mut self,
        summaries: Vec<WorkspaceChrome>,
        // One entry of tab rows per summary in the vertical tab-bar style,
        // empty in the horizontal one where the title bar still owns the tabs.
        tab_rows: Vec<Vec<AnyElement>>,
        renames: &InlineRenameSession,
        usage: SidebarUsage,
        // Connected paired hosts. With none, the list is just this
        // computer's workspaces; with some, it names them as local and
        // lists each host's sessions after them.
        remote: Vec<RemoteWorkspace>,
        window: &mut Window,
        cx: &mut Context<AppWindow>,
    ) -> AnyElement {
        self.list.end_cancelled_drag(cx);

        let local_title = match remote.is_empty() {
            true => t!("sidebar-workspaces-title"),
            false => t!("sidebar-local-workspaces-title"),
        };

        let remote_blocks = match remote.is_empty() {
            true => Vec::new(),
            false => remote_workspace_blocks(
                section_heading(t!("sidebar-remote-workspaces-title"), cx)
                    .pl(px(SIDEBAR_ROW_GUTTER))
                    .pt(px(SIDEBAR_GROUP_GAP))
                    .into_any_element(),
                &remote,
                &self.remote_folds,
                &self.remote_collapsed_hosts,
                renames,
                self.width,
                window,
                cx,
            ),
        };

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
                    .group("local-heading")
                    .w_full()
                    .justify_between()
                    .gap_1()
                    // A long heading ("Local Workspaces" at the narrowest
                    // width) gives way before the controls beside it do.
                    .child(section_heading(local_title, cx).min_w_0().truncate())
                    .child(
                        // Shown while the pointer is over the heading, like
                        // the controls of the rows below, so the list's top
                        // edge stays quiet. They keep their space when
                        // hidden, so the heading does not reflow on hover.
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .invisible()
                            .group_hover("local-heading", |this| this.visible())
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
            .child(self.list.render(
                &summaries,
                tab_rows,
                remote_blocks,
                renames,
                width,
                window,
                cx,
            ))
            .children((show_daily_token_usage || show_agent_usage).then(|| {
                v_flex()
                    .id("workspace-sidebar-status")
                    .map(Host::sidebar_status)
                    .w_full()
                    .flex_none()
                    .gap(px(SIDEBAR_STATUS_ROW_GAP))
                    .pt(px(SIDEBAR_STATUS_PADDING_TOP))
                    .pb(px(SIDEBAR_STATUS_PADDING_BOTTOM))
                    .border_t_1()
                    .border_color(cx.theme().sidebar_border)
                    .children(show_daily_token_usage.then(|| {
                        usage
                            .daily
                            .cached(StyleRefinement::default().w_full().h(px(STATUS_ROW_HEIGHT)))
                    }))
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

        // Until the first toggle, render at the resting width: no slide-in on
        // startup.
        sidebar_resize::slide_width(wrapper, "sidebar", !collapsed, px(width), self.animated)
    }
}

/// A section heading of the sidebar, set as a caps label so it is told apart
/// from the workspace names by case instead of by weight, which the names
/// use to mark the active one. Scripts without case are unchanged.
fn section_heading(label: impl AsRef<str>, cx: &App) -> Div {
    div()
        .map(Host::sidebar_heading)
        .text_size(px(SIDEBAR_SECTION_TEXT))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().sidebar_foreground.opacity(0.5))
        .child(label.as_ref().to_uppercase())
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
/// than the rows inside them, so a workspace and its tabs read as one block
/// instead of a flat list.
///
/// The panel owns the horizontal text inset; row fills extend into its gutter.
const SIDEBAR_PADDING_X: f32 = 12.0;

/// Where the host draws its window buttons over the title bar, the content
/// column starts under the close button's leading edge, so the heading, the
/// workspace names, the tab glyphs and the status icons stand on that edge
/// while the row fills reach past it into the gutter. Elsewhere the content
/// column starts where the app menu button's icon does, so the heading, the
/// tab glyphs and the status icons stand under that icon's edge.
const SIDEBAR_PADDING_LEFT: f32 = match Host::WINDOW_CONTROLS_INSET {
    Some(inset) => inset,
    None => Host::TITLE_BAR_LEADING_INSET + (Host::TITLE_BAR_BUTTON_SIZE - TOOLBAR_ICON_SIZE) / 2.0,
};

const SIDEBAR_GROUP_GAP: f32 = 8.0;

/// The heading uses one size across languages to keep the section easy to scan.
const SIDEBAR_SECTION_TEXT: f32 = 12.0;

/// The status cluster along the bottom edge: today's spend over the
/// subscription gauges. Both report what the agents have consumed, so they
/// stack as one block under a single rule instead of each drawing an edge.
const SIDEBAR_STATUS_PADDING_TOP: f32 = 8.0;

const SIDEBAR_STATUS_PADDING_BOTTOM: f32 = 2.0;
const SIDEBAR_STATUS_ROW_GAP: f32 = 2.0;

/// Distance from the row box's leading edge. The row is a rounded rectangle,
/// so a mark flush against that edge would sit outside the fill at the corners.
const SELECTION_BAR_INSET: f32 = 2.0;

/// Names retain an 8px gap after the selection mark on active and idle rows.
/// A host that marks the selected row by its fill alone has no mark to clear,
/// so its names start on the content column like the tab glyphs do.
const WORKSPACE_NAME_INSET: f32 = match Host::SIDEBAR_SELECTION_MARK {
    true => SELECTION_BAR_INSET + SELECTION_BAR_WIDTH + 8.0,
    false => SIDEBAR_ROW_GUTTER,
};

/// The two readouts the status cluster draws. The shell owns both, so their
/// refresh loops survive a sidebar collapse and a tab-bar style change.
pub(crate) struct SidebarUsage {
    pub(crate) daily: Entity<TokenUsageView>,
    pub(crate) quotas: Entity<AgentUsageView>,
}
