use std::collections;
use std::time::Duration;

use app::agent_tab::AgentKind;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Div, DragMoveEvent, ElementId, Role, SharedString, Stateful, Window,
    div, px,
};
use gpui_base::animation::ease_out_cubic;
use gpui_base::motion::{Transition, transition};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex};
use nmt_config::local_state::TabFold;
use nmt_platform::default_shell_name;
use nmt_terminal::event::ProgressReport;
use rust_i18n::t;

use crate::tabs::{TabId, TabManager};
use crate::ui::composition::{
    HoverActionLayout, HoverActionVisibility, StatusMark, StatusMarkTone, hover_action,
    motion_duration, progress_edge, sidebar_selection,
};
use crate::ui::remote::Remote;
use crate::ui::shell::{
    InlineRename, InlineRenameSession, InlineRenameStyle, TabSurface, pending_tab_icon,
};
use crate::ui::tab_bar::drag::{DragLabelPreview, DragStyle, TAB_ROW_HEIGHT};
use crate::ui::tab_bar::{progress_visual, viewers_mark};
use crate::ui::terminal_status::{terminal_dot, terminal_presentation};
use crate::ui::workspace_sidebar::SIDEBAR_ROW_GUTTER;
use crate::ui::{AppWindow, UI_RADIUS};
use crate::workspace::TerminalActivity;

/// Tabs listed as rows under their workspace. Rows can be reordered by drag
/// within their own workspace; the list keeps that gesture's state across
/// renders, and the sidebar places the rows it renders.
pub(crate) struct VerticalTabList {
    /// Row a drag currently hovers, keyed by workspace position and row
    /// position so rows of different workspaces cannot collide. That row
    /// shifts down to open an insertion gap. Only overwritten when the pointer
    /// enters another row: clearing on exit would oscillate, because opening
    /// the gap moves the hovered row out from under the pointer.
    drag_over: Option<(usize, usize)>,

    /// Source row hidden with zero opacity during a drag so its layout slot
    /// remains stable while the floating preview follows the pointer.
    dragging: Option<(usize, usize)>,
}

/// One workspace whose tabs the list renders.
pub(crate) struct WorkspaceTabs<'a> {
    /// Position of the workspace in the sidebar list.
    pub(crate) index: usize,

    pub(crate) tabs: &'a TabManager<TabSurface>,

    /// Every workspace keeps its own active tab, but only the active
    /// workspace's is the tab on screen.
    pub(crate) active: bool,

    /// Whether the workspace itself may be closed.
    pub(crate) closeable: bool,

    /// Which of its tabs the workspace lists.
    pub(crate) fold: TabFold,
}

/// A tab row picked up for reordering. The workspace position prevents a row
/// from being reordered into a different workspace's tab manager.
struct RowDrag {
    workspace: usize,
    from: usize,
    tab: TabId,
}

/// One tab's render inputs, snapshotted out of the tab manager before the
/// render closures borrow the shell.
struct TabRow {
    id: TabId,
    label: SharedString,
    active: bool,
    unread: bool,
    busy: bool,
    bell: bool,
    agent_kind: Option<AgentKind>,
    icon: Icon,

    /// Restored but not yet spawned.
    pending: bool,

    exited: bool,
    progress: Option<ProgressReport>,
    terminal: TerminalActivity,

    /// Names of the paired devices viewing the tab.
    viewers: Vec<String>,
}

/// Diameter of a tab row's status dot. Smaller than the workspace column's,
/// which keeps the two tiers apart at a glance.
const TAB_ROW_DOT: f32 = 7.0;

/// Spacing inside a tab row, between its glyph and its label.
pub(crate) const TAB_ROW_GAP: f32 = 6.0;

/// Edge of a tab row's type icon, and the size its label is set at.
pub(crate) const TAB_ROW_ICON: f32 = 14.0;

/// Edge of the glyph an `xsmall` icon draws inside the slot above. Every
/// icon centers its ink in this box, so a status dot centered in the same box
/// shares the icons' center line; the dot is narrower than their ink, and
/// aligning its leading edge with theirs instead would leave it visibly off
/// the column.
const TAB_ROW_GLYPH: f32 = 12.0;

const TAB_ROW_TEXT: f32 = 13.0;

impl VerticalTabList {
    pub(crate) fn new() -> Self {
        Self {
            drag_over: None,
            dragging: None,
        }
    }

    /// Close the make-way gap once the drag is gone without a drop on the
    /// list (cancelled via Escape, or released elsewhere). The cancel itself
    /// refreshes the window, so a call on every render always gets a chance
    /// to run.
    pub(crate) fn end_cancelled_drag(&mut self, cx: &Context<AppWindow>) {
        if !cx.has_active_drag() {
            self.drag_over = None;
            self.dragging = None;
        }
    }

    /// The rows of one workspace's tabs, in tab order. `row_width` is the
    /// width of the list column the rows fill, which the drag preview copies.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &self,
        workspace: WorkspaceTabs<'_>,
        unread_tabs: &collections::HashSet<TabId>,
        busy_agent_tabs: &collections::HashSet<TabId>,
        renames: &InlineRenameSession,
        row_width: f32,
        window: &mut Window,
        cx: &mut Context<AppWindow>,
    ) -> Vec<AnyElement> {
        let list = workspace.tabs.list();
        let active_id = list.active_id();

        let rows: Vec<TabRow> = list
            .items()
            .iter()
            .map(|tab| TabRow {
                id: tab.id(),
                label: match tab.title().is_empty() {
                    true => SharedString::new_static(default_shell_name()),
                    false => tab.title().to_string().into(),
                },
                // Marking the active tab of a workspace that is not on screen
                // would put a selection highlight on every workspace's list at
                // once.
                active: workspace.active && tab.id() == active_id,
                unread: unread_tabs.contains(&tab.id()),
                busy: busy_agent_tabs.contains(&tab.id()),
                bell: tab.bell(),
                agent_kind: tab.surface().agent_kind(cx),
                icon: tab.surface().icon(cx),
                pending: matches!(
                    tab.surface(),
                    TabSurface::Pending(_) | TabSurface::Hibernated(_)
                ),
                exited: tab.exited(),
                progress: tab.progress(),
                terminal: AppWindow::tab_terminal_activity(tab, cx),
                viewers: cx.global::<Remote>().viewers_of(tab.surface().pane_ids()),
            })
            .collect();

        // Closing a workspace's last tab falls through to closing the
        // workspace, so a row keeps its control as long as one of the two
        // would take effect. A pinned or sole workspace refuses both, and the
        // row withholds a control that would do nothing.
        let closeable = rows.len() > 1 || workspace.closeable;

        // Folding hides rows without renumbering them: a row keeps its tab's
        // position, which reordering and jumping to the tab work from.
        rows.iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let shown = match workspace.fold {
                    TabFold::All => true,
                    TabFold::Awake => !row.pending,
                    TabFold::Collapsed => false,
                };

                fold_row(
                    ("sidebar-tab-fold", row.id.0 as usize),
                    shown,
                    window,
                    cx,
                    |cx| {
                        self.render_row(
                            (workspace.index, index),
                            row,
                            closeable,
                            row_width,
                            renames,
                            cx,
                        )
                    },
                )
            })
            .collect()
    }

    /// One tab rendered as a row. Clicking it switches to that workspace *and*
    /// that tab, so a row under an inactive workspace is a single-click jump
    /// instead of a two-step one.
    fn render_row(
        &self,
        position: (usize, usize),
        tab: &TabRow,
        closeable: bool,
        row_width: f32,
        renames: &InlineRenameSession,
        cx: &mut Context<AppWindow>,
    ) -> AnyElement {
        let (ws_idx, tab_idx) = position;
        let tab_id = tab.id;
        let key = tab_id.0 as usize;
        let active = tab.active;

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
        // Ordered by what most needs action first: an agent mid-turn, then
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

        let row = tab_row(("sidebar-tab", key), tab.label.clone(), active, cx)
            .group("sidebar-tab")
            // A restored-but-not-yet-spawned tab renders faded, the same
            // "sleeping tab" cue the horizontal strip uses.
            .when(tab.pending, |this| this.opacity(0.6))
            // The status mark takes over the type icon's slot instead of
            // claiming a lane of its own ahead of it. What a session is doing
            // is what the eye scans the list for, and its kind only matters
            // once the row is found; sharing the slot also keeps the label
            // from shifting sideways the moment a tab starts working.
            .child(tab_row_icon(match (status_mark, tab.pending) {
                (Some(mark), _) => tab_row_glyph(mark).into_any_element(),
                (None, true) => pending_tab_icon(("sidebar-tab-pending", key)).into_any_element(),
                (None, false) => tab.icon.clone().into_any_element(),
            }))
            .child(label)
            // Bell dot, in the warning color so it reads apart from the unread
            // dot when a tab has both.
            .children(tab.bell.then(|| {
                StatusMark::new(
                    ("sidebar-tab-bell", key),
                    StatusMarkTone::Warning,
                    px(TAB_ROW_DOT),
                )
            }))
            .children(viewers_mark(("sidebar-tab-viewers", key), &tab.viewers, cx))
            .when(closeable, |this| this.child(close))
            .children(tab.progress.map(|report| {
                let (color, fraction) = progress_visual(report, cx);

                progress_edge(fraction, color)
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.jump_to_tab(ws_idx, tab_idx, window, cx);
            }));

        div()
            .id(("sidebar-tab-menu", key))
            .w_full()
            .when(self.dragging == Some(position), |this| this.opacity(0.0))
            // Make way for the dragged row: the hovered row slides down,
            // opening an insertion gap at the pointer.
            .when(self.drag_over == Some(position), |this| {
                this.mt(px(TAB_ROW_HEIGHT))
            })
            .on_drag(
                RowDrag {
                    workspace: ws_idx,
                    from: tab_idx,
                    tab: tab_id,
                },
                move |_, _, _, cx| {
                    drag_shell.update(cx, |this, cx| {
                        this.vertical_tabs_mut().dragging = Some(position);

                        cx.notify();
                    });

                    cx.new(|_| DragLabelPreview {
                        style: DragStyle::Sidebar,
                        label: drag_label.clone(),
                        width: row_width,
                    })
                },
            )
            .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<RowDrag>, _, cx| {
                if !e.bounds.contains(&e.event.position) {
                    return;
                }

                let drag = e.drag(cx);

                // No gap over the drag's own row, and none over another
                // workspace's rows, where the drop would be refused.
                let target = (drag.workspace == ws_idx && drag.from != tab_idx).then_some(position);

                if this.vertical_tabs_mut().drag_over != target {
                    this.vertical_tabs_mut().drag_over = target;

                    cx.notify();
                }
            }))
            .on_drop(cx.listener(move |this, drag: &RowDrag, window, cx| {
                // The list-level fallback handler must not also reorder this
                // drop.
                cx.stop_propagation();

                this.vertical_tabs_mut().drag_over = None;
                this.vertical_tabs_mut().dragging = None;

                if drag.workspace == ws_idx {
                    this.reorder_tab(drag.tab, drag.from, tab_idx, window, cx);
                }

                cx.notify();
            }))
            .modern_context_menu(move |menu, _, cx| {
                let rename_shell = menu_shell.clone();
                let close_shell = menu_shell.clone();
                let hibernate_shell = menu_shell.clone();
                let hibernatable = menu_shell.read(cx).can_hibernate_tab(tab_id, cx);

                menu.item(t!("tabbar-menu-rename"), move |window, cx| {
                    rename_shell.update(cx, |this, cx| this.start_tab_rename(tab_id, window, cx));
                })
                .icon(IconName::PenLine)
                .item_disabled(
                    t!("tabbar-menu-hibernate"),
                    !hibernatable,
                    move |window, cx| {
                        hibernate_shell.update(cx, |this, cx| {
                            this.request_hibernate_tab(tab_id, window, cx)
                        });
                    },
                )
                .icon(IconName::Moon)
                .item_disabled(t!("tabbar-menu-close"), !closeable, move |window, cx| {
                    close_shell.update(cx, |this, cx| this.request_close_tab(tab_id, window, cx));
                })
                .icon(IconName::Close)
            })
            .child(row)
            .into_any_element()
    }
}

/// The frame of a tab row in the sidebar, shared by this computer's tabs and
/// the sessions of paired hosts so the two lists read as one: its height,
/// inset, spacing, text, and the fill that marks the row on screen.
///
/// The row is the selectable thing in this style: its fill is the only cue
/// that a tab is the one on screen, so it exposes the selected state
/// assistive technology reads. The label is stated instead of derived,
/// because a row also holds status marks and swaps its text for an input
/// while its tab is being renamed.
pub(crate) fn tab_row(
    id: impl Into<ElementId>,
    label: SharedString,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let selection = sidebar_selection(cx);

    h_flex()
        .id(id)
        .role(Role::Tab)
        .aria_label(label)
        .aria_selected(active)
        .relative()
        .w_full()
        .h(px(TAB_ROW_HEIGHT))
        // The same inset the workspace rows take, so the glyph column stands
        // on the same edge as the workspace names above it. A tab is tied to
        // its workspace by the gap that separates one such block from the
        // next, not by an indent.
        .px(px(SIDEBAR_ROW_GUTTER))
        .gap(px(TAB_ROW_GAP))
        .items_center()
        .rounded(UI_RADIUS)
        .text_size(px(TAB_ROW_TEXT))
        // The selected row is marked by its fill alone. The fill already
        // separates the row from the list at a glance; adding an outline and a
        // heavier weight on top of it states the same thing three times, and
        // the weight change also reflows the label.
        .when(active, |this| {
            this.bg(selection.active_background)
                .text_color(selection.active_foreground)
        })
        .when(!active, |this| {
            this.text_color(selection.idle_foreground)
                .hover(|this| this.bg(selection.hover_background))
        })
}

/// The slot at a tab row's leading edge holding its type icon or status
/// mark. The glyph box starts on the slot's leading edge, which is the
/// content column, so icon ink lines up with the heading text; centering the
/// box in the wider slot would push it a pixel past.
pub(crate) fn tab_row_icon(glyph: impl IntoElement) -> Div {
    div()
        .flex_none()
        .size(px(TAB_ROW_ICON))
        .flex()
        .items_center()
        .justify_start()
        .child(glyph)
}

/// Centers a mark narrower than an icon's ink in the box icons draw their ink
/// in, so the mark stands on the icons' center line in the glyph column.
pub(crate) fn tab_row_glyph(mark: impl IntoElement) -> Div {
    div()
        .size(px(TAB_ROW_GLYPH))
        .flex()
        .items_center()
        .justify_center()
        .child(mark)
}

/// A row of a list its workspace folds. `shown` is whether the fold lists
/// it; a row the fold adds or removes grows or shrinks to its height while
/// fading, so the rows below slide instead of jumping. A row the fold keeps
/// out is not drawn once it has shrunk away, though its id still names its
/// motion so a later fold grows it from where it is. Rows appear at once
/// the first time they are drawn, so a new tab or a restored window does not
/// play the fold's motion.
pub(crate) fn fold_row(
    id: impl Into<ElementId>,
    shown: bool,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
    row: impl FnOnce(&mut Context<AppWindow>) -> AnyElement,
) -> Option<AnyElement> {
    fold_block(id, shown, TAB_ROW_HEIGHT, window, cx, |_, cx| row(cx))
}

/// A [`fold_row`] for a row of another height. The clip caps the row at
/// `height` instead of sizing it, so a row whose height follows its text
/// is never stretched: `height` only has to be at least the row's own, and
/// the closer it is, the sooner a fold starts to visibly shrink the row.
pub(crate) fn fold_block(
    id: impl Into<ElementId>,
    shown: bool,
    height: f32,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
    row: impl FnOnce(&mut Window, &mut Context<AppWindow>) -> AnyElement,
) -> Option<AnyElement> {
    let target = match shown {
        true => 1.0,
        false => 0.0,
    };

    let reveal: f32 = transition(
        (id.into(), "fold"),
        target,
        Transition::new(motion_duration(TAB_FOLD, cx)).ease(ease_out_cubic),
        window,
        cx,
    );

    if reveal <= 0.0 {
        return None;
    }

    let row = row(window, cx);

    // A settled row takes no clip, which would cut off the gap a dragged
    // row opens above it.
    if reveal >= 1.0 {
        return Some(row);
    }

    Some(
        div()
            .w_full()
            .max_h(px(height * reveal))
            .overflow_hidden()
            .opacity(reveal)
            .child(row)
            .into_any_element(),
    )
}

/// Long enough to follow where the rows went, short enough that clicking
/// through the folds is not held up by the list settling.
const TAB_FOLD: Duration = Duration::from_millis(180);

/// Fallback drop target for a list holding tab rows: a drop released over
/// the make-way gap (a margin, outside every row's hitbox) still drops at the
/// tracked insertion position instead of silently ending the drag.
pub(crate) fn accept_row_drops(list: Stateful<Div>, cx: &mut Context<AppWindow>) -> Stateful<Div> {
    list.on_drop(cx.listener(|this, drag: &RowDrag, window, cx| {
        this.vertical_tabs_mut().dragging = None;

        if let Some((ws, to)) = this.vertical_tabs_mut().drag_over.take()
            && drag.workspace == ws
        {
            this.reorder_tab(drag.tab, drag.from, to, window, cx);
        }

        cx.notify();
    }))
}
