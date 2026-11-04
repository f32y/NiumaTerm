#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;

use std::f32::consts::FRAC_PI_2;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, DragMoveEvent, ElementId, Entity, FontWeight,
    Pixels, ScrollHandle, SharedString, Stateful, Window, div, px, radians, relative,
    transparent_black,
};
use gpui_base::animation::ease_out_cubic;
use gpui_base::motion::{Transition, transition};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::scroll::Scrollbar;
use gpui_component::tooltip::ManagedTooltipExt as _;
use gpui_component::{ActiveTheme, Icon, IconName, IconNamed, Selectable, Sizable, h_flex, v_flex};
use nmt_config::appearance::TabBarStyle;
use nmt_config::local_state::TabFold;
use rust_i18n::t;

use crate::ui::composition::{
    HoverActionLayout, HoverActionVisibility, hover_action, motion_duration, progress_edge,
    sidebar_selection, toolbar_button,
};
use crate::ui::fluent::{SELECTION_BAR_HEIGHT, SELECTION_BAR_RADIUS, SELECTION_BAR_WIDTH};
use crate::ui::platform_style::{Host, PlatformStyle as _};
use crate::ui::shell::{InlineRename, InlineRenameSession, InlineRenameStyle};
use crate::ui::tab_bar::{
    TAB_ROW_GAP, TAB_ROW_ICON, accept_row_drops, new_tab_menu, tab_row_glyph, tab_row_icon,
};
use crate::ui::workspace_sidebar::drag::{WorkspaceDrag, WorkspaceDragPreview};
use crate::ui::workspace_sidebar::status::WorkspaceStatus;
use crate::ui::workspace_sidebar::{
    SELECTION_BAR_INSET, SIDEBAR_ROW_GUTTER, WORKSPACE_NAME_INSET, WorkspaceChrome,
};
use crate::ui::{AppSettings, AppWindow, UI_RADIUS, modern_dropdown};
use crate::workspace::{WorkspaceKind, workspace_display_label};

/// The scrolling list of workspaces, each heading the tab rows the vertical
/// tab-bar style places under it. Workspaces can be reordered by drag; the
/// list keeps that gesture's state and its scroll position across renders.
pub(super) struct WorkspaceList {
    scroll: ScrollHandle,

    /// Item position a workspace drag currently hovers: that item shifts down
    /// to open an insertion gap ("make way"). Only overwritten when the
    /// pointer enters another item; clearing on exit would oscillate, because
    /// opening the gap moves the hovered item out from under the pointer.
    drag_over: Option<usize>,

    /// Source item hidden with zero opacity during a drag so its layout slot
    /// remains stable while the floating preview follows the pointer.
    dragging: Option<usize>,
}

pub(super) const WORKSPACE_LIST_GAP: f32 = 6.0;

/// A workspace heading: its name, and the path that trails it on the same
/// line. The path is set small enough to read as an annotation on the name.
pub(super) const WORKSPACE_NAME_TEXT: f32 = 13.0;

pub(super) const WORKSPACE_PATH_TEXT: f32 = 10.5;

impl WorkspaceList {
    pub(super) fn new() -> Self {
        Self {
            scroll: ScrollHandle::new(),
            drag_over: None,
            dragging: None,
        }
    }

    /// Close the make-way gap once the drag is gone without a drop on the
    /// list (cancelled via Escape, or released elsewhere). The cancel itself
    /// refreshes the window, so a call on every render always gets a chance
    /// to run.
    pub(super) fn end_cancelled_drag(&mut self, cx: &Context<AppWindow>) {
        if !cx.has_active_drag() {
            self.drag_over = None;
            self.dragging = None;
        }
    }

    /// The list and its scrollbar. `tab_rows` holds one entry of tab rows per
    /// workspace in the vertical tab-bar style and is empty in the horizontal
    /// one, where the title bar owns the tabs. `width` is the sidebar width,
    /// which sets how much of each path fits.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render(
        &self,
        summaries: &[WorkspaceChrome],
        tab_rows: Vec<Vec<AnyElement>>,
        // The remote section, scrolling with the workspaces above it.
        remote_blocks: Vec<AnyElement>,
        renames: &InlineRenameSession,
        width: f32,
        window: &mut Window,
        cx: &mut Context<AppWindow>,
    ) -> AnyElement {
        let mut tab_rows = tab_rows.into_iter();

        // The scrollbar sits in this non-scrolling wrapper: an absolute child
        // of the scrolling list would be laid out against the content origin
        // and slide out of the viewport as the list scrolls.
        div()
            .relative()
            .flex_1()
            .min_h_0()
            // Stretch the wrapper over the leading gutter so the list retains
            // the header controls' trailing edge.
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
                    // hitbox) still drops at the tracked insertion position
                    // instead of silently ending the drag.
                    .on_drop(cx.listener(|this, drag: &WorkspaceDrag, window, cx| {
                        this.sidebar.list.dragging = None;

                        if let Some(to) = this.sidebar.list.drag_over.take() {
                            this.reorder_workspaces(drag.from, to, window, cx);
                        }

                        cx.notify();
                    }))
                    .map(|list| accept_row_drops(list, cx))
                    .children(summaries.iter().map(|ws| {
                        let tabs = tab_rows.next();

                        // A workspace heads its own tab rows, and the
                        // list gap is what separates one such block
                        // from the next; a rule between them would
                        // draw a second boundary inside the same gap.
                        let mut rows = Vec::new();

                        // Settings has one page and no directory grouping, so
                        // its tab is sufficient when tabs appear in this list.
                        if ws.summary.kind != WorkspaceKind::Settings || tabs.is_none() {
                            rows.push(self.render_row(ws.index, ws, renames, width, window, cx));
                        }

                        rows.extend(tabs.into_iter().flatten());

                        v_flex().w_full().children(rows)
                    }))
                    .children(remote_blocks),
            )
            .child(workspace_list_scrollbar(&self.scroll))
            .into_any_element()
    }

    /// One sidebar workspace item: a selectable button with busy indicator,
    /// name/cwd lines, hover-close, and a right-click menu (Rename / Close).
    /// While this workspace is being renamed (`rename` matches its id), the
    /// name line is replaced by the rename input.
    fn render_row(
        &self,
        idx: usize,
        chrome: &WorkspaceChrome,
        renames: &InlineRenameSession,
        width: f32,
        window: &mut Window,
        cx: &mut Context<AppWindow>,
    ) -> AnyElement {
        let ws = &chrome.summary;

        let settings_entry = ws.kind == WorkspaceKind::Settings;
        let selection = sidebar_selection(cx);

        // In the vertical tab-bar style every tab of this workspace is on
        // screen as its own row with its own status mark and progress, so
        // the workspace's aggregate of them would repeat the same state.
        let vertical_tabs =
            cx.global::<AppSettings>().config().appearance.tab_bar_style == TabBarStyle::Vertical;

        // The active tab's own row marks the selection in the vertical style,
        // so the workspace row takes it over only while that row is folded
        // away.
        let highlight_active = ws.active && (!vertical_tabs || ws.tab_fold == TabFold::Collapsed);

        let status = WorkspaceStatus {
            agent: chrome.agent.status,
            terminal: chrome.terminal_activity,
        };

        let status_label = status.label();

        // Runtime marks share the trailing controls so names keep a stable
        // leading edge in both tab layouts, including idle workspaces.
        let indicator = (!vertical_tabs).then(|| {
            status
                .column(("workspace-status", idx), ("workspace-busy", idx), cx)
                .into_any_element()
        });

        let ws_id = ws.id;

        let renaming = renames.workspace_input(ws_id).cloned();

        let controls: AnyElement = if vertical_tabs && !settings_entry {
            // This row heads the workspace's own tab list here, so its control
            // adds a tab to that list; closing moves to the context menu. The
            // press activates the workspace before the menu opens, so the
            // profile the user picks opens in the workspace they clicked
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
                this.activate_workspace(idx, window, cx);
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
        // payload once more, so they are built in the form those take instead
        // of being copied into it at each use.
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

        let path_budget = (width
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
        // path prefix, so it earns a trailing lane instead of a line of its
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
                // shows the name alone.
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

        // In the vertical style the row folds its tab list.
        let disclosure = vertical_tabs.then(|| {
            Disclosure::new(
                ("workspace-disclosure", ws_id.0 as usize),
                ws.tab_fold,
                window,
                cx,
            )
        });

        let drag_name = display_label.clone();
        let drag_cwd = display_path.clone();
        let drag_agent_status = chrome.agent.status;
        let drag_terminal_activity = chrome.terminal_activity;

        // Replicate the item's rendered width: sidebar width minus the card
        // gutter/border and the card's inner paddings around the list.
        let drag_width = (width - 36.0).max(80.0);

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
            // The name starts on the tab rows' icon column until the
            // disclosure mark makes it slide over.
            .when(vertical_tabs, |this| this.pl(px(SIDEBAR_ROW_GUTTER)))
            .child(
                h_flex()
                    .relative()
                    .w_full()
                    .gap_1p5()
                    .items_center()
                    .children(disclosure.as_ref().map(|disclosure| disclosure.mark(cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .when_some(disclosure.as_ref(), |this, disclosure| {
                                this.ml(disclosure.name_offset())
                            })
                            .child(name),
                    )
                    .child(suffix),
            )
            // In the vertical style the tab rows switch workspaces, so the
            // workspace row is left to fold them.
            .on_click(cx.listener(move |this, _, window, cx| match vertical_tabs {
                true => this.cycle_tab_fold(ws_id, cx),
                false => this.activate_workspace(idx, window, cx),
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
            .map(|fraction| progress_edge(fraction, cx.theme().primary));

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
                    this.sidebar.list.dragging = Some(idx);

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

                    if this.sidebar.list.drag_over != target {
                        this.sidebar.list.drag_over = target;

                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &WorkspaceDrag, window, cx| {
                // The list-level fallback handler must not also reorder this
                // drop.
                cx.stop_propagation();

                this.sidebar.list.drag_over = None;
                this.sidebar.list.dragging = None;

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
                // reading, so they lead as a row of buttons instead of taking a
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
            .child(match &disclosure {
                Some(disclosure) => disclosure
                    .hover_area(("workspace-hover", idx), item)
                    .into_any_element(),
                None => item.into_any_element(),
            })
            .when(!settings_entry, |row| {
                row.managed_tooltip_right(dirs_description)
            })
            // After the row itself, because the row's selected fill would
            // otherwise paint over the bar's lane.
            .children((highlight_active && Host::SIDEBAR_SELECTION_MARK).then(|| selection_bar(cx)))
            .children(progress)
            .into_any_element()
    }
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

pub(super) fn tail_preserving_path(path: &str, max_chars: usize) -> String {
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

/// The mark leading a workspace row whose click folds its tab list, and the
/// hover that reveals it. The fold already shows in the rows listed below,
/// so at rest the row shows no mark and its name stands on the tab rows'
/// icon column, which takes the row starting on the tab rows' inset. While
/// the pointer is on the row the mark fades in on that column and the name
/// slides right onto the tab labels' column to make room; both ease back
/// once the pointer leaves.
pub(super) struct Disclosure {
    fold: TabFold,

    /// Whether the pointer is on the row. Kept as element state, whose
    /// notification re-renders the window, so the hover handler retargets
    /// the reveal without the sidebar tracking every row.
    hovered: Entity<bool>,

    /// How far the reveal has eased, from 0 at rest to 1 under the pointer.
    reveal: f32,
}

impl Disclosure {
    /// `id` names the row across renders; the reveal keeps its progress
    /// under it, so a pointer leaving mid-way eases back from where it was.
    pub(super) fn new(
        id: impl Into<ElementId>,
        fold: TabFold,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let id = id.into();
        let hovered = window.use_keyed_state(id.clone(), cx, |_, _| false);

        let target = match *hovered.read(cx) {
            true => 1.0,
            false => 0.0,
        };

        let reveal = transition(
            (id, "reveal"),
            target,
            Transition::new(motion_duration(DISCLOSURE_REVEAL, cx)).ease(ease_out_cubic),
            window,
            cx,
        );

        Self {
            fold,
            hovered,
            reveal,
        }
    }

    /// Wrap `row` in the area whose hover drives the reveal. GPUI reports an
    /// element unhovered on every pointer move while a press on that same
    /// element is pending, and the row's own click and the trigger on it
    /// keep one pending for the whole click, so the slight pointer movement
    /// of a real click would ease the reveal out and back in. The wrapper
    /// takes no presses of its own, so its hover follows only where the
    /// pointer is. The pointer moves outside a frame, and notifying the
    /// state is what wakes the window to start the reveal.
    pub(super) fn hover_area(
        &self,
        id: impl Into<ElementId>,
        row: impl IntoElement,
    ) -> Stateful<Div> {
        let hovered = self.hovered.clone();

        div()
            .id(id)
            .w_full()
            .child(row)
            .on_hover(move |inside, _, cx| {
                hovered.update(cx, |hovered, cx| {
                    if *hovered != *inside {
                        *hovered = *inside;

                        cx.notify();
                    }
                });
            })
    }

    /// How far the name stands right of the icon column.
    pub(super) fn name_offset(&self) -> Pixels {
        px(self.reveal * (TAB_ROW_ICON + TAB_ROW_GAP))
    }

    /// The mark, drawn over the start of the row's content so it takes no
    /// room of its own: the name's offset makes that room. A filled triangle
    /// says how the way a tree's disclosure mark does: right while the tabs
    /// are folded away, down while any are listed. It sits in the tab rows'
    /// glyph slot so it stands on their icon column.
    pub(super) fn mark(&self, cx: &App) -> Div {
        let icon = Icon::new(DisclosureIcon)
            .with_size(px(DISCLOSURE_SIZE))
            .text_color(cx.theme().sidebar_foreground.opacity(0.5));

        let icon = match self.fold {
            TabFold::Collapsed => icon,
            TabFold::All => icon.rotate(radians(FRAC_PI_2)),
        };

        div()
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .flex()
            .items_center()
            .opacity(self.reveal)
            .child(tab_row_icon(tab_row_glyph(icon)))
    }
}

/// Long enough to read as the name making way for the mark instead of
/// jumping, short enough that sweeping the pointer down the list leaves no
/// trail of rows still settling.
const DISCLOSURE_REVEAL: Duration = Duration::from_millis(150);

/// Edge of the disclosure triangle, small enough to read as a mark on the
/// name, not as a control of its own.
const DISCLOSURE_SIZE: f32 = 8.0;

/// The filled triangle that marks a workspace's tab fold
/// (`assets/icons/disclosure.svg`), drawn pointing right.
struct DisclosureIcon;

impl IconNamed for DisclosureIcon {
    fn path(self) -> SharedString {
        "icons/disclosure.svg".into()
    }
}

/// Sidebar pinned-workspace glyph (`assets/icons/pin.svg`).
struct PinIcon;

impl IconNamed for PinIcon {
    fn path(self) -> SharedString {
        "icons/pin.svg".into()
    }
}

/// The accent bar that marks the selected row. It is drawn out of the row's
/// flow so it can sit in the gutter left of the row's own padding, and it
/// paints the accent color on its own: the row fill stays a neutral subtle
/// wash, which keeps a selected row legible against a translucent pane.
pub(super) fn selection_bar(cx: &App) -> impl IntoElement {
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
                .color(transparent_black())
                .hover(selection.hover_background)
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
