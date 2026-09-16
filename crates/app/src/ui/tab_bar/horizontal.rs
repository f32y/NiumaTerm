#[cfg(test)]
#[path = "horizontal_tests.rs"]
mod tests;

use std::{cell, collections, rc};

use app::agent_tab::AgentKind;
use app::design::{SETTINGS_NAV_WIDTH, SURFACE_RADIUS, TAB_HEIGHT};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DragMoveEvent, IsZero as _, MouseButton, Pixels, ScrollHandle,
    SharedString, div, px, relative,
};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::tab::{Tab, TabBar, TabVariant};
use gpui_component::{ActiveTheme, ElementExt as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use nmt_config::appearance::TabShape;
use nmt_terminal::event::ProgressReport;
use rust_i18n::t;

use crate::tabs::{TabId, TabManager};
use crate::ui::composition::{
    HoverActionLayout, HoverActionVisibility, StatusMark, StatusMarkTone, TOOLBAR_BUTTON_SIZE,
    hover_action, toolbar_button,
};
use crate::ui::platform_style::{Host, PlatformStyle as _, TabDensity};
use crate::ui::shell::{
    InlineRename, InlineRenameSession, InlineRenameStyle, TabSurface, pending_tab_icon,
};
use crate::ui::tab_bar::drag::{DragLabelPreview, DragStyle, TabDrag};
use crate::ui::tab_bar::menu::new_tab_menu;
use crate::ui::tab_bar::progress_visual;
use crate::ui::terminal_status::{TerminalVisual, terminal_dot, terminal_presentation};
use crate::ui::{AppSettings, AppWindow, UI_RADIUS, modern_dropdown};
use crate::workspace::TerminalActivity;

pub(crate) struct TabStrip {
    /// Scroll position of the tab strip (tabs overflow horizontally once their
    /// fixed widths exceed the bar).
    scroll: ScrollHandle,

    /// Active tab at the last render; a change scrolls the new active tab into
    /// view (render-time compare-and-set, so every switch path counts).
    last_active: Option<TabId>,

    /// True right after the startup reveal request; the request is repeated on
    /// the second render because the scroll handle drops requests made before
    /// its first prepaint.
    reveal_retry: bool,

    /// Destination tab and trailing-edge marker. The marker never changes
    /// tab bounds, so hovering it cannot move the destination away.
    drag_over: Option<(usize, bool)>,

    /// Strip width recorded during the previous prepaint, which is what
    /// `Auto Size` divides between the tabs. Held in a cell because the
    /// measurement arrives from a prepaint callback, long after `render` has
    /// given up its borrow.
    measured_width: rc::Rc<cell::Cell<f32>>,
}

/// Narrowest a tab gets under `Auto Size`: one glyph slot centered in the
/// pill's content padding, plus the gap and borders the tab draws around it
/// (2 borders + 32 padding + 16 slot + 4 gap).
const MIN_AUTO_TAB_WIDTH: f32 = 54.0;

impl TabStrip {
    pub(crate) fn new() -> Self {
        Self {
            scroll: ScrollHandle::new(),
            last_active: None,
            reveal_retry: false,
            drag_over: None,
            measured_width: rc::Rc::new(cell::Cell::new(0.0)),
        }
    }

    /// Scroll the newly active tab into view on a switch. Any switch path
    /// re-renders the shell, so this render-time compare-and-set catches every
    /// path without fighting manual scrolling on unrelated re-renders. On the
    /// very first render the scroll handle hasn't recorded its overflow axes yet
    /// (that happens in its first prepaint), so the request is consumed as a
    /// no-op — re-request once on the second render via `reveal_retry`.
    pub(crate) fn reveal_active(
        &mut self,
        active_id: TabId,
        active_index: usize,
        cx: &mut Context<AppWindow>,
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

    pub(crate) fn render(
        &self,
        tabs: &TabManager<TabSurface>,
        unread_tabs: &collections::HashSet<TabId>,
        busy_agent_tabs: &collections::HashSet<TabId>,
        renames: &InlineRenameSession,
        cx: &mut Context<AppWindow>,
    ) -> AnyElement {
        let active_idx = tabs.list().active_index();

        let items: Vec<TabItem> = tabs
            .list()
            .items()
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
                agent_kind: tab.surface().agent_kind(cx),
                icon: tab.surface().icon(cx),
                bell: tab.bell(),
                pending: matches!(tab.surface(), TabSurface::Pending(_)),
                exited: tab.exited(),
                progress: tab.progress(),
                terminal: AppWindow::tab_terminal_activity(tab, cx),
            })
            .collect();

        // `+` right after the last tab opens the new-tab menu: one entry per
        // configured terminal profile, plus one per agent profile.
        // Ctrl+Shift+T still opens the default profile directly.
        let menu_shell = cx.entity();

        let new_tab = modern_dropdown(
            toolbar_button("tab-new").icon(IconName::Plus),
            move |menu, _, cx| new_tab_menu(menu, &menu_shell, cx),
        );

        let tab_count = items.len();

        // The settings entry presents one tab and no way to add another, so
        // its lone tab keeps a close control the tab strip would normally
        // withhold from a single tab.
        let settings_workspace = tabs.active().is_settings();
        let closeable = tab_count > 1 || settings_workspace;
        let shell = cx.entity();

        // Width from the Appearance setting; long titles clip inside the tab's
        // own overflow_hidden. Auto Size treats that value as the upper bound
        // and divides the strip between the tabs instead.
        let settings = cx.global::<AppSettings>();
        let configured_width = settings.config().appearance.tab_width as f32;
        let auto_size = settings.config().appearance.tab_auto_size;
        let tab_shape = settings.config().appearance.tab_shape;

        let tab_width = if settings_workspace {
            SETTINGS_NAV_WIDTH.into()
        } else if auto_size {
            auto_tab_width(
                self.measured_width.get(),
                tab_count,
                configured_width,
                tab_shape,
            )
        } else {
            configured_width
        };

        let density = tab_density(tab_width);
        let icon_only = density == TabDensity::IconOnly;

        let bar = TabBar::new("shell-tabs")
            .map(|bar| match tab_shape {
                // Large gives a 30px pill row, an easier click and drag target
                // than the compact 24px one while leaving the terminal below
                // its room.
                TabShape::Rounded => bar.with_variant(TabVariant::Modern).large(),
                // Attached tabs sit on the content edge, which already draws
                // the line a bar baseline would repeat.
                TabShape::Attached => bar.with_variant(TabVariant::Tab).bottom_border(false),
            })
            // The leading region already ends on the content's edge, so the
            // variant's own leading padding would set the first tab apart
            // from the content below it.
            .pl_0()
            .w_full()
            .min_w_0()
            .selected_index(active_idx)
            // Overflowing tabs scroll horizontally; the handle lets the shell
            // scroll the active tab into view on switches.
            .track_scroll(&self.scroll)
            // Empty title-bar space remains draggable; only the control blocks
            // the drag hitbox behind it. The settings entry holds exactly one
            // tab, so it offers no way to add a second.
            .when(!settings_workspace, |this| {
                this.inline_suffix(div().occlude().child(new_tab))
            })
            .children(items.into_iter().enumerate().map(|(index, item)| {
                let TabItem {
                    id,
                    label,
                    unread,
                    busy,
                    agent_kind,
                    icon,
                    bell,
                    pending,
                    exited,
                    progress,
                    terminal,
                } = item;

                // `×` closes this tab; `stop_propagation` keeps the click from
                // also activating the tab (the TabBar's on_click). Shown only
                // while the tab is hovered (visibility keeps its width, so the
                // tab doesn't reflow) - except on the active tab once only one
                // glyph fits, where the control has to stay up because the
                // hover state it would otherwise wait for is already the one
                // the tab is in.
                let close_pinned = icon_only && index == active_idx;

                let close_layout = match icon_only {
                    true => HoverActionLayout::Fill,
                    false => HoverActionLayout::Inline,
                };

                let close_visibility = match close_pinned {
                    true => HoverActionVisibility::Always,
                    false => HoverActionVisibility::OnGroupHover("shell-tab".into()),
                };

                let close = hover_action(
                    ("tab-close", id as usize),
                    t!("tabbar-menu-close"),
                    close_layout,
                    close_visibility,
                    "×",
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();

                    this.request_close_tab(TabId(id), window, cx);
                }))
                .into_any_element();

                // A tab down to one glyph hands that slot to the close control
                // on hover; wider tabs keep it at the trailing edge.
                let (slot_close, suffix_close) = if icon_only {
                    (Some(close), None)
                } else {
                    (None, Some(close))
                };

                let suffix = div()
                    .relative()
                    .h_full()
                    .flex()
                    .items_center()
                    .map(|suffix| Host::tab_suffix(suffix, density))
                    .children(suffix_close)
                    .children(progress.map(|report| progress_bar(report, tab_width, cx)));

                // Inline rename: the label swaps for an input. The mouse-down
                // stopper keeps clicks in the input from activating the tab
                // (and blurring the input); Escape cancels before the input
                // sees it. Otherwise the label carries the right-click menu.
                let renaming = renames.tab_input(TabId(id)).cloned();

                let content: AnyElement = if let Some(input) = renaming {
                    let rename_shell = cx.entity();

                    InlineRename::new(
                        ("tab-rename", id as usize),
                        label.clone(),
                        input,
                        InlineRenameStyle::HorizontalTab,
                        move |window, cx| {
                            rename_shell
                                .update(cx, |this, cx| this.finish_tab_rename(false, window, cx));
                        },
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
                        // keeping the title clipped with ellipsis.
                        .flex_1()
                        .h_full()
                        .flex()
                        .items_center()
                        .map(|title| Host::tab_title(title, density))
                        .overflow_hidden()
                        .modern_context_menu(move |menu, _, _| {
                            let rename_shell = menu_shell.clone();
                            let close_shell = menu_shell.clone();

                            menu.item(t!("tabbar-menu-rename"), move |window, cx| {
                                rename_shell.update(cx, |this, cx| {
                                    this.start_tab_rename(TabId(id), window, cx)
                                });
                            })
                            .icon(IconName::PenLine)
                            .item_disabled(
                                t!("tabbar-menu-close"),
                                !closeable,
                                move |window, cx| {
                                    close_shell.update(cx, |this, cx| {
                                        this.request_close_tab(TabId(id), window, cx)
                                    });
                                },
                            )
                            .icon(IconName::Close)
                        })
                        .gap_1()
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
                        .map(|this| {
                            if icon_only {
                                // One glyph, two states stacked in the same
                                // slot: the tab's icon at rest, the close
                                // control while the pointer is on the tab. The
                                // slot is sized for the click target rather
                                // than the 12px glyph, and the pill's own
                                // padding frames it on both sides.
                                return this.child(
                                    div()
                                        .relative()
                                        .flex_none()
                                        .size_4()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div()
                                                .flex()
                                                .when(close_pinned, |this| this.invisible())
                                                .when(!close_pinned, |this| {
                                                    this.group_hover("shell-tab", |this| {
                                                        this.invisible()
                                                    })
                                                })
                                                .child(if pending {
                                                    pending_tab_icon((
                                                        "tab-pending-icon",
                                                        id as usize,
                                                    ))
                                                    .into_any_element()
                                                } else {
                                                    icon.clone().into_any_element()
                                                }),
                                        )
                                        .children(slot_close),
                                );
                            }

                            // A title with only a few characters of room left
                            // renders as an ellipsis and little else, so the
                            // narrower tab spends that width on the icon and
                            // the close control instead.
                            this.when(density == TabDensity::Full, |this| {
                                this.child(div().truncate().child(label.clone()))
                            })
                            // Unread-notification dot: a filled circle
                            // instead of a text bullet, so it stays visible
                            // against the muted inactive-tab text. An agent
                            // notification nobody has read reports the same
                            // "this tab wants you" state a finished command
                            // does, so it takes the success color the
                            // terminal dot already uses.
                            .children((unread && agent_kind.is_none()).then(|| {
                                StatusMark::new(
                                    ("tab-unread", id as usize),
                                    StatusMarkTone::Success,
                                    px(TAB_DOT),
                                )
                                .label(
                                    t!("sidebar-workspace-unread-label", count = "1").into_owned(),
                                )
                            }))
                            // Bell dot, in the warning color so it reads
                            // apart from the unread dot when a tab has
                            // both.
                            .children(bell.then(|| {
                                StatusMark::new(
                                    ("tab-bell", id as usize),
                                    StatusMarkTone::Warning,
                                    px(TAB_DOT),
                                )
                            }))
                        })
                        .into_any_element()
                };

                let drag_label: SharedString = label.into();

                // `occlude` keeps the platform from treating the tab as
                // title-bar drag area, so clicks and the wheel reach the
                // client instead of starting a window move. That same
                // blocking hides the strip's scroll container from the hit
                // test, so the wheel is forwarded to its scroll handle here;
                // prepaint clamps the offset to the scrollable range.
                let scroll = self.scroll.clone();

                shell_tab(tab_shape)
                    .aria_label(drag_label.clone())
                    .map(|tab| Host::tab(tab, density))
                    .on_scroll_wheel(move |event, window, _| {
                        let delta = event.delta.pixel_delta(window.line_height());

                        let step = if delta.x.is_zero() { delta.y } else { delta.x };

                        if step.is_zero() {
                            return;
                        }

                        let mut offset = scroll.offset();

                        offset.x += step;

                        scroll.set_offset(offset);

                        window.refresh();
                    })
                    .w(px(tab_width))
                    .relative()
                    .when_some(
                        self.drag_over.filter(|(target, _)| *target == index),
                        |this, (_, after)| {
                            this.child(
                                div()
                                    .absolute()
                                    .top_1()
                                    .bottom_1()
                                    .w(px(2.0))
                                    .bg(cx.theme().primary)
                                    .when(after, |this| this.right_0())
                                    .when(!after, |this| this.left_0()),
                            )
                        },
                    )
                    .when(agent_kind.is_none() && !icon_only, |this| {
                        // Laid out like the agent prefix below, so a terminal
                        // tab's icon sits at the same offset whether or not a
                        // command is running and never shifts when one starts.
                        this.prefix(
                            div()
                                .map(Host::tab_prefix)
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(if pending {
                                    pending_tab_icon(("tab-pending-icon", id as usize))
                                        .into_any_element()
                                } else {
                                    icon.clone().into_any_element()
                                })
                                .when_some(
                                    terminal_presentation(terminal),
                                    |this, (visual, label)| {
                                        this.child(
                                            div()
                                                .id(("tab-terminal-indicator", id as usize))
                                                .aria_label(label)
                                                .size_4()
                                                .flex_none()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                // A running command gets the
                                                // same spinner an agent tab
                                                // shows mid-turn: in the strip
                                                // both mean "this tab is still
                                                // working", and a static dot
                                                // there read as one more
                                                // colored state rather than as
                                                // motion. A finished command
                                                // keeps its graded dot, since
                                                // that state no longer moves.
                                                .child(match visual {
                                                    TerminalVisual::Running => StatusMark::busy((
                                                        "tab-terminal-busy-spinner",
                                                        id as usize,
                                                    ))
                                                    .into_any_element(),
                                                    _ => terminal_dot(visual, TAB_DOT, cx),
                                                }),
                                        )
                                    },
                                ),
                        )
                    })
                    .when_some(agent_kind.filter(|_| !icon_only), |this, _| {
                        let indicator = agent_tab_indicator(busy, unread);

                        this.prefix(
                            div()
                                .map(Host::tab_prefix)
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(if pending {
                                    pending_tab_icon(("tab-pending-icon", id as usize))
                                        .into_any_element()
                                } else {
                                    icon.clone().into_any_element()
                                })
                                .when_some(indicator, |this, indicator| {
                                    this.child(
                                        div()
                                            .id(("tab-agent-indicator", id as usize))
                                            .aria_label(match indicator {
                                                AgentTabIndicator::Busy => {
                                                    t!("tabbar-tooltip-agent-busy")
                                                }
                                                AgentTabIndicator::Ready => {
                                                    t!("tabbar-tooltip-agent-ready")
                                                }
                                            })
                                            .size_4()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(match indicator {
                                                AgentTabIndicator::Busy => StatusMark::busy((
                                                    "tab-agent-busy-spinner",
                                                    id as usize,
                                                ))
                                                .into_any_element(),
                                                // An agent waiting on the user
                                                // is the same "this tab is
                                                // done working" state a
                                                // finished command reports, so
                                                // it takes the success color
                                                // the terminal dot already
                                                // uses rather than a second
                                                // color for one meaning.
                                                AgentTabIndicator::Ready => StatusMark::new(
                                                    ("tab-agent-ready", id as usize),
                                                    StatusMarkTone::Success,
                                                    px(TAB_DOT),
                                                )
                                                .label(t!("tabbar-tooltip-agent-ready"))
                                                .into_any_element(),
                                            }),
                                    )
                                }),
                        )
                    })
                    .child(content)
                    .suffix(suffix)
                    .group("shell-tab")
                    // Drag a tab to reorder it; drop maps the source position
                    // (`from`) onto this tab's position.
                    .on_drag(TabDrag { from: index }, move |_, _, _, cx| {
                        cx.new(|_| DragLabelPreview {
                            style: DragStyle::Tab,
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
                        let from = e.drag(cx).from;
                        let target = (from != index).then_some((index, from < index));

                        if this.tab_strip_mut().drag_over != target {
                            this.tab_strip_mut().drag_over = target;

                            cx.notify();
                        }
                    }))
                    .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                        // The strip-level fallback handler must not
                        // also reorder this drop.
                        cx.stop_propagation();

                        this.tab_strip_mut().drag_over = None;

                        this.workspaces
                            .active_tabs_mut()
                            .list_mut()
                            .reorder(drag.from, index);

                        this.focus_active(window, cx);

                        this.sync_session_memory(cx);

                        cx.notify();
                    }))
            }))
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                this.workspaces.active_tabs_mut().list_mut().activate(*ix);

                this.show_active_tab(window, cx);
            }));

        // Releasing over the strip's trailing space keeps the last visible
        // destination, without inserting layout margins between tabs.
        let measured_width = self.measured_width.clone();
        let measured_shell = shell.clone();

        div()
            .id("tab-strip-drop")
            .w_full()
            .min_w_0()
            // Auto Size needs the width the strip actually got, which layout
            // only settles after this render. Recording it and asking for one
            // more render converges in a single extra frame, and the equality
            // guard keeps that from repeating every frame.
            .when(auto_size, |this| {
                this.on_prepaint(move |bounds, _, cx| {
                    let width: f32 = bounds.size.width.into();

                    if measured_width.get() != width {
                        measured_width.set(width);

                        // `Window::refresh` is a no-op mid-draw, so the redraw
                        // is requested through the shell entity instead.
                        measured_shell.update(cx, |_, cx| cx.notify());
                    }
                })
            })
            .on_drop(cx.listener(|this, drag: &TabDrag, window, cx| {
                if let Some((to, _)) = this.tab_strip_mut().drag_over.take() {
                    this.workspaces
                        .active_tabs_mut()
                        .list_mut()
                        .reorder(drag.from, to);

                    this.focus_active(window, cx);

                    this.sync_session_memory(cx);
                }

                cx.notify();
            }))
            .child(bar)
            .into_any_element()
    }
}

fn tab_density(tab_width: f32) -> TabDensity {
    if tab_width >= Host::FULL_TAB_WIDTH {
        TabDensity::Full
    } else if tab_width >= Host::COMPACT_TAB_WIDTH {
        TabDensity::Compact
    } else {
        TabDensity::IconOnly
    }
}

/// Gap the bar leaves between neighbouring tabs, and around the whole strip.
/// `TabVariant::Modern` fixes both at 4px while attached tabs have neither,
/// and the tab widths have to be reduced by that much to keep the row from
/// overflowing.
fn tab_gap(shape: TabShape) -> f32 {
    match shape {
        TabShape::Rounded => 4.0,
        TabShape::Attached => 0.0,
    }
}

/// Room held back for the trailing new-tab button, which shares the row with
/// the tabs.
const NEW_TAB_BUTTON_WIDTH: f32 = TOOLBAR_BUTTON_SIZE;

/// Width one tab takes under `Auto Size`. Tabs hold `configured` while the row
/// has room and then shrink together, never past the point where the leading
/// icon would be clipped. Below that the row overflows and the strip's
/// horizontal scroll takes over.
fn auto_tab_width(strip_width: f32, tab_count: usize, configured: f32, shape: TabShape) -> f32 {
    let floor = MIN_AUTO_TAB_WIDTH.min(configured);

    // A strip that has never been laid out reports no width. Starting from the
    // configured width keeps the first frame at full size rather than flashing
    // every tab down to the floor and back.
    if tab_count == 0 || strip_width <= 0.0 {
        return configured;
    }

    // One gap per tab: between neighbours, plus one before the new-tab button.
    // The strip keeps only its trailing padding.
    let gap = tab_gap(shape);
    let reserved = gap + NEW_TAB_BUTTON_WIDTH + gap * tab_count as f32;
    let share = (strip_width - reserved) / tab_count as f32;

    if share.is_finite() {
        share.clamp(floor, configured)
    } else {
        configured
    }
}

/// One tab's render inputs, snapshotted out of the manager before the closure
/// borrows the shell.
struct TabItem {
    id: u64,
    label: String,
    unread: bool,
    busy: bool,
    agent_kind: Option<AgentKind>,
    icon: Icon,
    bell: bool,

    /// Restored but not yet spawned.
    pending: bool,

    exited: bool,
    progress: Option<ProgressReport>,
    terminal: TerminalActivity,
}

/// Diameter of a tab's status dot, matching the unread and bell marks that
/// share the strip.
const TAB_DOT: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentTabIndicator {
    Busy,
    Ready,
}

fn agent_tab_indicator(busy: bool, unread: bool) -> Option<AgentTabIndicator> {
    if busy {
        Some(AgentTabIndicator::Busy)
    } else if unread {
        Some(AgentTabIndicator::Ready)
    } else {
        None
    }
}

fn progress_bar_width(tab_width: Pixels) -> Pixels {
    (tab_width - UI_RADIUS * 2.0).max(Pixels::ZERO)
}

/// Progress bar along the bottom edge of a tab, driven by OSC 9;4. One corner
/// radius of space at each side keeps the track on the straight bottom edge.
fn progress_bar(report: ProgressReport, tab_width: f32, cx: &App) -> AnyElement {
    let (color, fraction) = progress_visual(report, cx);

    div()
        .absolute()
        .bottom_0()
        .right(UI_RADIUS)
        .w(progress_bar_width(px(tab_width)))
        .h(px(2.0))
        .child(
            div()
                .h_full()
                .w(relative(fraction))
                .rounded_full()
                .bg(color),
        )
        .into_any_element()
}

/// Native hit testing and the title bar's mouse handlers both leave tab
/// gestures to the tab, including movement before the reorder threshold.
/// Pills take the variant's radius on all four corners, while an attached tab
/// rounds only the corners away from the content it rests on.
fn shell_tab(shape: TabShape) -> Tab {
    let tab = match shape {
        TabShape::Rounded => Tab::new(),
        TabShape::Attached => Tab::new().top_corner_radius(SURFACE_RADIUS),
    };

    tab.occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// Width of each tab in [`tab_shape_preview`].
const PREVIEW_TAB_WIDTH: f32 = 96.0;

/// An active and an inactive tab over a band of content surface, drawn with
/// the strip's metrics and theme colors. Plain elements register no hitbox,
/// so the preview takes no hover styles and leaves every click to the picker
/// row that holds it; a real `Tab` swallows the left mouse press.
pub(crate) fn tab_shape_preview(shape: TabShape, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let gap = px(tab_gap(shape));

    let tab = |active: bool, title_width: f32| {
        let fill = match active {
            true => theme.tab_active,
            false => theme.transparent,
        };

        let tab = div()
            .w(px(PREVIEW_TAB_WIDTH))
            .flex()
            .items_center()
            .justify_center()
            .bg(fill)
            .child(
                div()
                    .w(px(title_width))
                    .h(px(4.0))
                    .rounded_full()
                    .bg(theme.muted_foreground.opacity(0.5)),
            );

        // `TabVariant::Modern` at the large size draws a 30px pill outlined
        // with the sidebar border while selected. `TabVariant::Tab` draws a
        // full-height tab whose side and top borders show while selected.
        match (shape, active) {
            (TabShape::Rounded, true) => tab
                .h(px(30.0))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.sidebar_border),
            (TabShape::Rounded, false) => tab.h(px(30.0)).rounded(theme.radius),
            (TabShape::Attached, true) => tab
                .h(TAB_HEIGHT)
                .rounded_t(SURFACE_RADIUS)
                .border_x_1()
                .border_t_1()
                .border_color(theme.border),
            (TabShape::Attached, false) => tab.h(TAB_HEIGHT),
        }
    };

    let bar_fill = match shape {
        TabShape::Rounded => theme.transparent,
        TabShape::Attached => theme.tab_bar,
    };

    v_flex()
        .w_full()
        .overflow_hidden()
        .rounded(UI_RADIUS)
        .border_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
        .child(
            h_flex()
                .p(gap)
                .gap(gap)
                .bg(bar_fill)
                .child(tab(true, 44.0))
                .child(tab(false, 32.0)),
        )
        .child(div().h(px(12.0)).bg(theme.tab_active))
        .into_any_element()
}
