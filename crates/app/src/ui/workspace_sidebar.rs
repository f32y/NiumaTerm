use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, Context, DragMoveEvent, ElementId, Entity, KeyDownEvent,
    MouseButton, ScrollHandle, SharedString, StatefulInteractiveElement, Window, div, px, relative,
};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu as _, PopupMenuItem};
use gpui_component::progress::ProgressCircle;
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{ActiveTheme, Icon, IconName, IconNamed, Selectable, Sizable, h_flex, v_flex};
use nmt_agent_utils::AgentRuntimeStatus;
use nmt_i18n::i18n;
use nmt_terminal::event::ProgressReport;

use super::{AppSettings, NewWorkspace, Shell};
use crate::agent_pane::AgentKind;
use crate::agent_pane::usage::AgentUsageView;
use crate::tabs::TabId;
use crate::ui::sidebar_resize::{self, ResizeDrag};
use crate::ui::tab_bar::{new_tab_menu, progress_visual, tab_icon};
use crate::ui::terminal_status::{terminal_dot, terminal_presentation};
use crate::ui::{UI_RADIUS, floating_surface};
use crate::window::WindowRegistry;
use crate::workspace::{TerminalActivity, WorkspaceId, WorkspaceKind, WorkspaceSummary};

/// Default expanded width of the workspace sidebar, in pixels; the user can
/// drag the right edge to resize.
pub(super) const SIDEBAR_WIDTH: f32 = 180.0;
/// Drag limits: keep the workspace list readable and leave room for the terminal.
/// Handle id for this column's resize grip. Every resizable column receives
/// every other column's drag-move events, so this is what distinguishes them.
pub(super) const RESIZE_HANDLE: &str = "workspace-sidebar-resize";

pub(super) const MIN_WIDTH: f32 = 140.0;
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

    let dot = |color| {
        div()
            .size(px(STATUS_DOT))
            .rounded_full()
            .bg(color)
            .into_any_element()
    };

    let glyphs = agent
        .map(|(visual, _)| match visual {
            AgentVisual::Running => ProgressCircle::new(busy_id)
                .small()
                .loading(true)
                .color(cx.theme().warning)
                .into_any_element(),
            AgentVisual::NeedsInput => dot(cx.theme().primary),
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

/// A tab row picked up for reordering. The workspace travels with it so a drop
/// on another workspace's rows can be refused: moving a tab between workspaces
/// means moving it between tab managers, which reordering cannot express.
struct SidebarTabDrag {
    workspace: usize,
    from: usize,
    /// Identifies the tab manager to reorder, which the row position alone
    /// cannot: positions repeat across workspaces.
    tab: TabId,
}

/// The floating preview under the cursor while a tab row is dragged: the row's
/// label on an opaque fill, because the ghost floats over arbitrary content.
struct SidebarTabDragPreview {
    label: SharedString,
    width: f32,
}

impl Render for SidebarTabDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w(px(self.width))
            .h(px(TAB_ROW_HEIGHT))
            .px_2()
            .items_center()
            .rounded(UI_RADIUS)
            .overflow_hidden()
            .text_xs()
            .bg(cx
                .theme()
                .background
                .blend(cx.theme().sidebar)
                .blend(cx.theme().sidebar_accent))
            .text_color(cx.theme().sidebar_accent_foreground)
            .child(div().truncate().child(self.label.clone()))
    }
}

/// Height of a tab row. A workspace item stacks a name and a path line, so a
/// row stays visibly shorter than one and the two tiers read as ranked, while
/// leaving the row a comfortable click target.
const TAB_ROW_HEIGHT: f32 = 30.0;

/// Diameter of a tab row's status dot. Smaller than the workspace column's,
/// which keeps the two tiers apart at a glance.
const TAB_ROW_DOT: f32 = 6.0;

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
    terminal_activity: TerminalActivity,
    width: f32,
}

impl Render for WorkspaceDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (glyphs, status_label) = workspace_status_glyphs(
            self.agent_status,
            self.terminal_activity,
            "workspace-drag-busy",
            cx,
        );

        let indicator = v_flex()
            .id("workspace-drag-status")
            .aria_label(status_label)
            .w_4()
            .flex_none()
            .gap_0p5()
            .items_center()
            .justify_center()
            .children(glyphs);

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
            .rounded(UI_RADIUS)
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
        let settings_entry = ws.kind == WorkspaceKind::Settings;
        let (glyphs, status_label) = workspace_status_glyphs(
            ws.agent_status,
            ws.terminal_activity,
            ("workspace-busy", idx),
            cx,
        );

        let indicator = v_flex()
            .id(("workspace-status", idx))
            .aria_label(status_label.clone())
            // The column's width is fixed so an idle workspace can suppress its
            // glyphs without shifting its name relative to active neighbours;
            // the height follows its contents so a stacked pair centers as a
            // group and a lone glyph centers on its own.
            .w_4()
            .flex_none()
            .gap_0p5()
            .items_center()
            .justify_center()
            .children(glyphs)
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
                    .aria_label(
                        i18n("sidebar-workspace-unread-label")
                            .replace("{count}", &ws.unread_count.to_string()),
                    )
                    .size_5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .child(ws.unread_count.to_string())
            }))
            .child(controls);

        let full_path = ws.cwd.clone();
        let display_path = tail_preserving_path(
            &full_path,
            (((self.width - 80.0) / 7.0).floor() as usize).clamp(8, 64),
        );
        // A temporary workspace wears the same `*` an unsaved document does,
        // so its absence from the next session is visible before the user
        // closes the window.
        let display_label = match ws.temporary {
            true => format!("* {}", workspace_display_label(&ws.name, &ws.cwd)),
            false => workspace_display_label(&ws.name, &ws.cwd),
        };
        let name = div()
            .id(("workspace-secondary", idx))
            .aria_label(display_label.clone())
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
            name.child(display_label.clone()).into_any_element()
        };

        let drag_name: SharedString = display_label.clone().into();
        let drag_cwd: SharedString = display_path.clone().into();
        let drag_agent_status = ws.agent_status;
        let drag_terminal_activity = ws.terminal_activity;

        // Replicate the item's rendered width: sidebar width minus the card
        // gutter/border and the card's inner paddings around the list.
        let drag_width = (self.width - 36.0).max(80.0);
        let item = Button::new(("workspace", idx))
            .ghost()
            .when(!settings_entry, |this| this.tooltip(full_path.clone()))
            .aria_label(if settings_entry {
                display_label.clone()
            } else {
                i18n("sidebar-workspace-item-label")
                    .replace("{name}", &display_label)
                    .replace("{path}", &full_path)
                    .replace("{status}", &status_label)
            })
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
                                    .id(("workspace-path", idx))
                                    .w_full()
                                    .text_left()
                                    .text_xs()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .when(!settings_entry, |this| {
                                        this.aria_label(full_path.clone())
                                    })
                                    .text_color(cx.theme().sidebar_foreground.opacity(0.6))
                                    // The settings entry has no working
                                    // directory. A blank run still forms a line
                                    // box, so its row stands as tall as the
                                    // workspaces around it.
                                    .child(if settings_entry {
                                        SharedString::new_static(" ")
                                    } else {
                                        display_path.into()
                                    }),
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
        let pin_label = if pinned {
            i18n("sidebar-workspace-menu-unpin")
        } else {
            i18n("sidebar-workspace-menu-pin")
        };
        let cwd = ws.cwd.clone();
        let temporary = ws.temporary;

        let progress = ws
            .progress
            .fraction()
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
            .context_menu(move |menu, _, _| {
                let rename_shell = shell.clone();
                let close_shell = shell.clone();
                let pin_shell = shell.clone();
                let activate_shell = shell.clone();
                let cwd = cwd.clone();

                // Renaming, pinning, and copying a path all describe a
                // workspace the user owns; the settings entry is dismissible
                // and nothing else.
                let menu = if settings_entry {
                    menu
                } else {
                    menu.item(
                        PopupMenuItem::new(i18n("sidebar-workspace-menu-rename")).on_click(
                            move |_, window, cx| {
                                rename_shell.update(cx, |this, cx| {
                                    this.start_workspace_rename(ws_id, window, cx)
                                });
                            },
                        ),
                    )
                    .item(PopupMenuItem::new(pin_label).on_click(move |_, _, cx| {
                        pin_shell
                            .update(cx, |this, cx| this.set_workspace_pinned(ws_id, !pinned, cx));
                    }))
                    .item(
                        PopupMenuItem::new(i18n("sidebar-workspace-menu-copy-path")).on_click(
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(cwd.clone()));
                            },
                        ),
                    )
                    // Only a temporary workspace has anything to adopt.
                    .when(temporary, |menu| {
                        menu.item(
                            PopupMenuItem::new(i18n("sidebar-workspace-menu-activate")).on_click(
                                move |_, _, cx| {
                                    activate_shell.update(cx, |this, cx| {
                                        this.activate_as_workspace(ws_id, cx)
                                    });
                                },
                            ),
                        )
                    })
                };

                menu.item(
                    PopupMenuItem::new(i18n("sidebar-workspace-menu-close"))
                        .disabled(!closeable)
                        .on_click(move |_, window, cx| {
                            close_shell.update(cx, |this, cx| {
                                this.request_close_workspace(ws_id, window, cx)
                            });
                        }),
                )
            })
            .child(item)
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
        rename: Option<&(TabId, Entity<InputState>)>,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        let tab_id = tab.id;
        let key = tab_id.0 as usize;
        let active = tab.active;

        let close = div()
            .id(("sidebar-tab-close", key))
            .px_1()
            .invisible()
            .group_hover("sidebar-tab", |this| this.visible())
            .child("\u{00d7}")
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.request_close_tab(tab_id, window, cx);
            }));

        let dot = |color| {
            div()
                .flex_none()
                .size(px(TAB_ROW_DOT))
                .rounded_full()
                .bg(color)
        };

        // The agent's own marks replace the generic unread dot, the way the
        // horizontal strip does it: a spinner while the turn runs, an accent
        // dot once it has something to read.
        let agent_mark: Option<AnyElement> = match (tab.agent_kind.is_some(), tab.busy, tab.unread)
        {
            (true, true, _) => Some(
                ProgressCircle::new(("sidebar-tab-busy", key))
                    .small()
                    .loading(true)
                    .color(cx.theme().warning)
                    .into_any_element(),
            ),
            (_, _, true) => Some(dot(cx.theme().primary).into_any_element()),
            _ => None,
        };

        let renaming = rename
            .filter(|(id, _)| *id == tab_id)
            .map(|(_, input)| input.clone());

        let label: AnyElement = match renaming {
            Some(input) => div()
                .flex_1()
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                    if e.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.finish_tab_rename(false, window, cx);
                    }
                }))
                .child(
                    Input::new(&input)
                        .xsmall()
                        .p_0()
                        .text_xs()
                        .appearance(false),
                )
                .into_any_element(),
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
        // The row spans the list column: sidebar width minus the card gutter,
        // the card's inner padding, and the scrollbar lane.
        let drag_width = (self.width - 36.0).max(80.0);

        let row = h_flex()
            .id(("sidebar-tab", key))
            .group("sidebar-tab")
            .relative()
            .w_full()
            .h(px(TAB_ROW_HEIGHT))
            // Indent past the workspace item's status column so the rows read
            // as belonging to the workspace above them.
            .pl_6()
            .pr_1()
            .gap_1()
            .items_center()
            .rounded(UI_RADIUS)
            .text_xs()
            // A restored-but-not-yet-spawned tab renders faded, the same
            // "sleeping tab" cue the horizontal strip uses.
            .when(tab.pending, |this| this.opacity(0.6))
            .when(active, |this| {
                this.bg(cx.theme().sidebar_accent)
                    .text_color(cx.theme().sidebar_accent_foreground)
            })
            .when(!active, |this| {
                this.text_color(cx.theme().sidebar_foreground.opacity(0.75))
                    .hover(|this| this.bg(cx.theme().sidebar_accent.opacity(0.4)))
            })
            .child(div().flex_none().flex().child(match tab.pending {
                true => Icon::new(IconName::Moon).xsmall().into_any_element(),
                false => tab_icon(tab.agent_kind, tab.settings).into_any_element(),
            }))
            .when_some(
                terminal_presentation(tab.terminal),
                |this, (visual, aria)| {
                    this.child(
                        div()
                            .id(("sidebar-tab-terminal", key))
                            .aria_label(aria)
                            .flex_none()
                            .flex()
                            .child(terminal_dot(visual, TAB_ROW_DOT, cx)),
                    )
                },
            )
            .child(label)
            .children(agent_mark)
            // Bell dot, in the warning color so it reads apart from the unread
            // dot when a tab carries both.
            .children(tab.bell.then(|| dot(cx.theme().warning)))
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
                this.workspaces.activate(ws_idx);
                this.workspaces.active_tabs_mut().activate(tab_idx);
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
                    cx.new(|_| SidebarTabDragPreview {
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
            .context_menu(move |menu, _, _| {
                let rename_shell = menu_shell.clone();
                let close_shell = menu_shell.clone();

                menu.item(PopupMenuItem::new(i18n("tabbar-menu-rename")).on_click(
                    move |_, window, cx| {
                        rename_shell
                            .update(cx, |this, cx| this.start_tab_rename(tab_id, window, cx));
                    },
                ))
                .item(
                    PopupMenuItem::new(i18n("tabbar-menu-close"))
                        .disabled(!closeable)
                        .on_click(move |_, window, cx| {
                            close_shell
                                .update(cx, |this, cx| this.request_close_tab(tab_id, window, cx));
                        }),
                )
            })
            .child(row)
            .into_any_element()
    }

    /// The new-tab row that closes out the active workspace's tab list. Only
    /// the active workspace gets one: the title bar's `+` is gone in this
    /// style, and a new tab always opens where the user is looking. Clicking it
    /// opens the same profile menu the horizontal strip's `+` does, so the two
    /// styles offer the same choices; Ctrl+Shift+T still opens the default
    /// profile directly.
    fn render_new_tab_row(&self, cx: &mut Context<Shell>) -> AnyElement {
        let menu_shell = cx.entity();

        Button::new("sidebar-tab-new")
            .ghost()
            .aria_label(i18n("sidebar-tab-new"))
            .w_full()
            .h(px(TAB_ROW_HEIGHT))
            .px_0()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(cx.theme().sidebar_foreground.opacity(0.6))
                    .child("+"),
            )
            .dropdown_menu(move |menu, _, cx| new_tab_menu(menu, &menu_shell, cx))
            .into_any_element()
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
        agent_usage: Entity<AgentUsageView>,
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

        // Fixed-width content; the animated wrapper below clips it so the buttons
        // don't reflow while the sidebar slides. The transparent panel inherits
        // the window background while the drag and animation math keeps operating
        // on the full `width`.
        let panel = div()
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .p_2()
            .gap_1()
            .child(
                Button::new("new-workspace")
                    .ghost()
                    .label(i18n("sidebar-workspace-new"))
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
                    .on_drop(cx.listener(|this, drag: &SidebarTabDrag, window, cx| {
                        this.sidebar.tab_dragging = None;

                        if let Some((ws, to)) = this.sidebar.tab_drag_over.take() {
                            if drag.workspace == ws {
                                this.reorder_tab(drag.tab, drag.from, to, window, cx);
                            }
                        }

                        cx.notify();
                    }))
                    .children(summaries.iter().enumerate().flat_map(|(idx, ws)| {
                        let mut rows = vec![self.render_item(idx, ws, rename, cx)];
                        let ws_tabs = tabs.get(idx).map(Vec::as_slice).unwrap_or_default();
                        // A workspace refuses to close its last tab, so the
                        // row withholds the control the manager would ignore.
                        let closeable = ws_tabs.len() > 1;

                        rows.extend(ws_tabs.iter().enumerate().map(|(tab_idx, tab)| {
                            self.render_tab_row(idx, tab_idx, tab, closeable, tab_rename, cx)
                        }));

                        if ws.active && !ws_tabs.is_empty() {
                            rows.push(self.render_new_tab_row(cx));
                        }

                        rows
                    }))
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
                    .child(agent_usage)
            }));

        // The terminal column's left gutter forms the gap between panels and
        // keeps the resize handle at the panel edge, so no right inset is needed.
        let content = div()
            .w(px(width))
            .h_full()
            .pl(px(floating_surface::SIDE_INSET))
            .pt(px(floating_surface::TOP_INSET))
            .pb(px(floating_surface::BOTTOM_INSET))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::CommandOutcome;

    #[test]
    fn generated_workspace_uses_final_cwd_component() {
        assert_eq!(
            workspace_display_label("New Workspace", r"C:\Workspace\NiumaTerm\"),
            "NiumaTerm"
        );
        assert_eq!(
            workspace_display_label("Renamed", r"C:\Workspace\NiumaTerm"),
            "Renamed"
        );
        assert_eq!(
            workspace_display_label("New Workspace", "."),
            "New Workspace"
        );
    }

    #[test]
    fn long_workspace_path_keeps_its_tail() {
        assert_eq!(
            tail_preserving_path(r"C:\very\long\workspace\NiumaTerm", 18),
            "…\\NiumaTerm"
        );
        assert_eq!(tail_preserving_path("short/path", 18), "short/path");
    }

    #[test]
    fn an_idle_workspace_supplies_no_glyph_but_retains_semantics() {
        // Rendering owns the column; this state projection verifies that an
        // idle workspace contributes no glyph while keeping a spoken label.
        assert_eq!(agent_presentation(AgentRuntimeStatus::Idle), None);
        assert_eq!(terminal_presentation(TerminalActivity::Idle), None);
        assert_eq!(status_column_label(None, None), "Idle");
    }

    #[test]
    fn both_halves_of_the_column_are_spoken_together() {
        let (agent, agent_label) = agent_presentation(AgentRuntimeStatus::NeedsInput).unwrap();
        let (_, terminal_label) =
            terminal_presentation(TerminalActivity::Finished(CommandOutcome::Failed)).unwrap();

        assert_eq!(agent, AgentVisual::NeedsInput);
        assert_eq!(
            status_column_label(Some(agent_label), Some(terminal_label)),
            "Needs input, Command failed"
        );
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
