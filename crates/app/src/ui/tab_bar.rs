use std::{cell, collections, rc};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DragMoveEvent, Entity, IsZero as _, KeyDownEvent, MouseButton,
    Pixels, Render, ScrollHandle, SharedString, Window, div, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu as _, PopupMenuItem};
use gpui_component::progress::ProgressCircle;
use gpui_component::tab::{Tab, TabBar, TabVariant};
use gpui_component::{ActiveTheme, ElementExt as _, Icon, IconName, Sizable};
use nmt_i18n::i18n;
use nmt_terminal::event::{ProgressReport, ProgressState};

use super::Shell;
use super::shell::TabSurface;
use crate::agent_pane::AgentKind;
use crate::agent_pane::usage::{ClaudeIcon, CodexIcon};
use crate::tabs::{TabId, TabManager};
use crate::ui::{AppSettings, UI_RADIUS};

struct TabDrag {
    from: usize,
}

/// The floating preview shown under the cursor while dragging a tab: a
/// full-size replica of the active tab pill. The pill's alpha fill is
/// composited onto the chrome background here, because the ghost floats over
/// arbitrary content where a bare alpha fill would wash out.
struct TabDragPreview {
    label: SharedString,
    width: f32,
}

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().rounded(UI_RADIUS).bg(cx.theme().background).child(
            div()
                .w(px(self.width))
                .h(px(30.0))
                .px_2()
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .rounded(UI_RADIUS)
                .bg(cx.theme().tab_active)
                .text_sm()
                .text_color(cx.theme().tab_active_foreground)
                .child(div().truncate().child(self.label.clone())),
        )
    }
}

pub(super) struct TabStrip {
    /// Scroll position of the tab strip (tabs overflow horizontally once their
    /// fixed widths exceed the bar).
    pub(super) scroll: ScrollHandle,
    /// Active tab at the last render; a change scrolls the new active tab into
    /// view (render-time compare-and-set, so every switch path counts).
    last_active: Option<TabId>,
    /// True right after the startup reveal request; the request is repeated on
    /// the second render because the scroll handle drops requests made before
    /// its first prepaint.
    reveal_retry: bool,
    /// Tab position a tab drag currently hovers: that tab shifts right to open
    /// an insertion gap ("make way"). Only overwritten when the pointer enters
    /// another tab — clearing on exit would oscillate, because opening the gap
    /// moves the hovered tab out from under the pointer.
    drag_over: Option<usize>,
    /// Strip width recorded during the previous prepaint, which is what
    /// `Auto Size` divides between the tabs. Held in a cell because the
    /// measurement arrives from a prepaint callback, long after `render` has
    /// given up its borrow.
    measured_width: rc::Rc<cell::Cell<f32>>,
}

/// How far a tab slides to open the insertion gap while a drag hovers it.
const TAB_MAKE_WAY_PX: f32 = 32.0;

/// Narrowest a tab gets under `Auto Size`: one glyph slot centered in the
/// pill's content padding, plus the gap and borders the tab draws around it
/// (2 borders + 32 padding + 16 slot + 4 gap).
const MIN_AUTO_TAB_WIDTH: f32 = 54.0;

/// Below this a tab can no longer stand the leading icon, the pill's content
/// padding and the close control side by side (2 + 12 + 4 + 32 + 4 + 16), so
/// it collapses to the single glyph slot.
const COMPACT_TAB_WIDTH: f32 = 70.0;

/// Below this the title has under four characters of room left over from the
/// icon, the padding and the close control, which renders as an ellipsis and
/// little else, so the tab spends the width on the two controls instead.
const FULL_TAB_WIDTH: f32 = 100.0;

/// What a tab still has room to draw. The close control outranks the tab
/// icon, which outranks the title: a tab nobody can close is worse than a tab
/// nobody can identify at a glance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabDensity {
    /// Icon, title, and the close control on hover.
    Full,
    /// Icon and the close control on hover; the title is dropped.
    Compact,
    /// A single glyph slot, shared by the icon and the close control.
    IconOnly,
}

fn tab_density(tab_width: f32) -> TabDensity {
    if tab_width >= FULL_TAB_WIDTH {
        TabDensity::Full
    } else if tab_width >= COMPACT_TAB_WIDTH {
        TabDensity::Compact
    } else {
        TabDensity::IconOnly
    }
}

/// Gap the tab bar leaves between neighbouring pills, and around the whole
/// strip. `TabVariant::Modern` fixes both at 4px, and the tab widths have to
/// be reduced by that much to keep the row from overflowing.
const TAB_GAP: f32 = 4.0;
const TAB_BAR_PADDING: f32 = TAB_GAP * 2.0;

/// Room held back for the trailing new-tab button, which shares the row with
/// the tabs.
const NEW_TAB_BUTTON_WIDTH: f32 = 28.0;

/// Width one tab takes under `Auto Size`. Tabs hold `configured` while the row
/// has room and then shrink together, never past the point where the leading
/// icon would be clipped. Below that the row overflows and the strip's
/// horizontal scroll takes over.
fn auto_tab_width(strip_width: f32, tab_count: usize, configured: f32) -> f32 {
    let floor = MIN_AUTO_TAB_WIDTH.min(configured);

    // A strip that has never been laid out reports no width. Starting from the
    // configured width keeps the first frame at full size rather than flashing
    // every tab down to the floor and back.
    if tab_count == 0 || strip_width <= 0.0 {
        return configured;
    }

    // One gap per tab: between neighbours, plus one before the new-tab button.
    let reserved = TAB_BAR_PADDING + NEW_TAB_BUTTON_WIDTH + TAB_GAP * tab_count as f32;
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
    settings: bool,
    bell: bool,
    /// Restored but not yet spawned.
    pending: bool,
    exited: bool,
    progress: Option<ProgressReport>,
}

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

/// The glyph a tab leads with: the agent's own mark on an agent tab, a gear on
/// the settings tab, a terminal mark otherwise.
fn tab_icon(agent_kind: Option<AgentKind>, settings: bool) -> Icon {
    match agent_kind {
        Some(AgentKind::Codex) => Icon::new(CodexIcon),
        Some(AgentKind::Claude) => Icon::new(ClaudeIcon),
        None if settings => Icon::new(IconName::Settings),
        None => Icon::new(IconName::SquareTerminal),
    }
    .xsmall()
}

fn progress_bar_width(tab_width: Pixels) -> Pixels {
    (tab_width - UI_RADIUS * 2.0).max(Pixels::ZERO)
}

/// Progress bar along the bottom edge of a tab, driven by OSC 9;4. One corner
/// radius of space at each side keeps the track on the straight bottom edge.
fn progress_bar(report: ProgressReport, tab_width: f32, cx: &App) -> AnyElement {
    let percent = |default: u8| report.progress.unwrap_or(default) as f32 / 100.0;

    let (color, fraction) = match report.state {
        ProgressState::Set => (cx.theme().primary, percent(0)),
        ProgressState::Error => (cx.theme().danger, percent(100)),
        ProgressState::Pause => (cx.theme().warning, percent(100)),
        // Indeterminate reports carry no percentage: a full-width muted bar
        // reads as "running, no ETA" and stays distinguishable from a finished
        // determinate bar, which is full-width in the accent color. No pulse
        // animation — the strip would then repaint every frame for as long as
        // any background command runs.
        ProgressState::Indeterminate => (cx.theme().muted_foreground, 1.0),
        ProgressState::Remove => (cx.theme().primary, 0.0),
    };

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

impl TabStrip {
    pub(super) fn new() -> Self {
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
    pub(super) fn reveal_active(
        &mut self,
        active_id: TabId,
        active_index: usize,
        cx: &mut Context<Shell>,
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

    pub(super) fn render(
        &self,
        tabs: &TabManager<TabSurface>,
        unread_tabs: &collections::HashSet<TabId>,
        busy_agent_tabs: &collections::HashSet<TabId>,
        rename: Option<&(TabId, Entity<InputState>)>,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        let active_idx = tabs.active_index();

        let items: Vec<TabItem> = tabs
            .tabs()
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
                settings: tab.surface().is_settings(),
                bell: tab.bell(),
                pending: matches!(tab.surface(), TabSurface::Pending(_)),
                exited: tab.exited(),
                progress: tab.progress(),
            })
            .collect();

        // `+` right after the last tab opens the new-tab menu: one entry per
        // configured terminal profile, plus one per agent profile.
        // Ctrl+Shift+T still opens the default profile directly.
        let menu_shell = cx.entity();
        let new_tab = Button::new("tab-new")
            .ghost()
            .px_2()
            .child("+")
            .dropdown_menu(move |menu, _, cx| {
                let mut menu = menu;

                for profile in cx.global::<AppSettings>().profiles.clone() {
                    let shell_cmd = profile.shell.trim().to_string();

                    // A profile without a command cannot spawn; offering it
                    // would silently fall back to the built-in shell.
                    if shell_cmd.is_empty() {
                        continue;
                    }

                    let args: Vec<String> = profile
                        .args
                        .split_whitespace()
                        .map(str::to_string)
                        .collect();
                    let item_shell = menu_shell.clone();

                    menu = menu.item(
                        PopupMenuItem::new(profile.name.clone())
                            .icon(tab_icon(None, false))
                            .on_click(move |_, window, cx| {
                                let launch = (Some(shell_cmd.clone()), args.clone());
                                item_shell.update(cx, |this, cx| {
                                    this.open_profile_tab(launch, window, cx)
                                });
                            }),
                    );
                }

                let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();

                // No separator over an empty agent section (every agent
                // profile deleted).
                if !agent_profiles.is_empty() {
                    menu = menu.separator();
                }

                for (ix, profile) in agent_profiles.into_iter().enumerate() {
                    let label = if profile.name.trim().is_empty() {
                        i18n("tabbar-menu-agent-profile").replace("{index}", &(ix + 1).to_string())
                    } else {
                        profile.name.clone()
                    };
                    let item_shell = menu_shell.clone();

                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .icon(tab_icon(Some(AgentKind::from_profile(profile.kind)), false))
                            .on_click(move |_, window, cx| {
                                let profile = profile.clone();
                                item_shell.update(cx, |this, cx| {
                                    this.open_agent_tab(profile, window, cx)
                                });
                            }),
                    );
                }

                menu
            });

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
        let configured_width = settings.tab_width as f32;
        let auto_size = settings.tab_auto_size;
        let tab_width = if auto_size {
            auto_tab_width(self.measured_width.get(), tab_count, configured_width)
        } else {
            configured_width
        };
        let density = tab_density(tab_width);
        let icon_only = density == TabDensity::IconOnly;

        let bar = TabBar::new("shell-tabs")
            // Soft-rounded pills floating on the chrome (VS Code Modern UI
            // look); Large gives a 30px strip, taller than the compact 24px one
            // for an easier click/drag target while leaving the terminal below
            // its room.
            .with_variant(TabVariant::Modern)
            .large()
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
                    settings: is_settings,
                    bell,
                    pending,
                    exited,
                    progress,
                } = item;
                // `×` closes this tab; `stop_propagation` keeps the click from
                // also activating the tab (the TabBar's on_click). Shown only
                // while the tab is hovered (visibility keeps its width, so the
                // tab doesn't reflow) - except on the active tab once only one
                // glyph fits, where the control has to stay up because the
                // hover state it would otherwise wait for is already the one
                // the tab is in.
                let close_pinned = icon_only && index == active_idx;
                let close = div()
                    .id(("tab-close", id as usize))
                    .when(!icon_only, |this| this.px_1())
                    // In the glyph slot the control *is* the slot: covering it
                    // keeps the click target as large as the icon it hides,
                    // instead of shrinking to the width of the `×` itself.
                    .when(icon_only, |this| {
                        this.absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                    })
                    .when(!close_pinned, |this| {
                        this.invisible()
                            .group_hover("shell-tab", |this| this.visible())
                    })
                    .child("×")
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
                    .children(suffix_close)
                    .children(progress.map(|report| progress_bar(report, tab_width, cx)));

                // Inline rename: the label swaps for an input. The mouse-down
                // stopper keeps clicks in the input from activating the tab
                // (and blurring the input); Escape cancels before the input
                // sees it. Otherwise the label carries the right-click menu.
                let renaming = rename
                    .filter(|(rid, _)| *rid == TabId(id))
                    .map(|(_, input)| input.clone());

                let content: AnyElement = if let Some(input) = renaming {
                    div()
                        .flex_1()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                            if e.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.finish_tab_rename(false, window, cx);
                            }
                        }))
                        .child(
                            Input::new(&input)
                                .small()
                                .p_0()
                                .text_center()
                                .appearance(false),
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
                        // keeping the label centered and clipped with ellipsis.
                        .flex_1()
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .overflow_hidden()
                        .context_menu(move |menu, _, _| {
                            let rename_shell = menu_shell.clone();
                            let close_shell = menu_shell.clone();

                            menu.item(PopupMenuItem::new(i18n("tabbar-menu-rename")).on_click(
                                move |_, window, cx| {
                                    rename_shell.update(cx, |this, cx| {
                                        this.start_tab_rename(TabId(id), window, cx)
                                    });
                                },
                            ))
                            .item(
                                PopupMenuItem::new(i18n("tabbar-menu-close"))
                                    .disabled(!closeable)
                                    .on_click(move |_, window, cx| {
                                        close_shell.update(cx, |this, cx| {
                                            this.request_close_tab(TabId(id), window, cx)
                                        });
                                    }),
                            )
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
                                                .child(tab_icon(agent_kind, is_settings)),
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
                            // Unread-notification dot: a filled accent
                            // circle instead of a text bullet, so it stays
                            // visible against the muted inactive-tab text.
                            .children((unread && agent_kind.is_none()).then(|| {
                                div()
                                    .flex_none()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(cx.theme().primary)
                            }))
                            // Bell dot, in the warning color so it reads
                            // apart from the unread dot when a tab has
                            // both.
                            .children(bell.then(|| {
                                div()
                                    .flex_none()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(cx.theme().warning)
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
                Tab::new()
                    .occlude()
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
                    // Make way for the dragged tab: the hovered tab
                    // slides right, opening an insertion gap at the
                    // pointer.
                    .when(self.drag_over == Some(index), |this| {
                        this.ml(px(TAB_MAKE_WAY_PX))
                    })
                    .when(agent_kind.is_none() && !icon_only, |this| {
                        this.prefix(
                            div()
                                .relative()
                                .left(px(8.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .child(tab_icon(None, is_settings)),
                        )
                    })
                    .when_some(agent_kind.filter(|_| !icon_only), |this, agent_kind| {
                        let indicator = agent_tab_indicator(busy, unread);
                        this.prefix(
                            div()
                                .relative()
                                .left(px(4.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(tab_icon(Some(agent_kind), false))
                                .when_some(indicator, |this, indicator| {
                                    this.child(
                                        div()
                                            .id(("tab-agent-indicator", id as usize))
                                            .aria_label(match indicator {
                                                AgentTabIndicator::Busy => {
                                                    i18n("tabbar-tooltip-agent-busy")
                                                }
                                                AgentTabIndicator::Ready => {
                                                    i18n("tabbar-tooltip-agent-ready")
                                                }
                                            })
                                            .size_4()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(match indicator {
                                                AgentTabIndicator::Busy => ProgressCircle::new((
                                                    "tab-agent-busy-spinner",
                                                    id as usize,
                                                ))
                                                .small()
                                                .loading(true)
                                                .color(cx.theme().warning)
                                                .into_any_element(),
                                                AgentTabIndicator::Ready => div()
                                                    .size(px(6.0))
                                                    .rounded_full()
                                                    .bg(cx.theme().primary)
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
                        cx.new(|_| TabDragPreview {
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
                        let target = (e.drag(cx).from != index).then_some(index);

                        if this.tab_strip.drag_over != target {
                            this.tab_strip.drag_over = target;
                            cx.notify();
                        }
                    }))
                    .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                        // The strip-level fallback handler must not
                        // also reorder this drop.
                        cx.stop_propagation();

                        this.tab_strip.drag_over = None;
                        this.workspaces.active_tabs_mut().reorder(drag.from, index);

                        this.focus_active(window, cx);
                        this.sync_session_memory(cx);

                        cx.notify();
                    }))
            }))
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                this.workspaces.active_tabs_mut().activate(*ix);

                this.focus_active(window, cx);
                this.sync_session_memory(cx);

                cx.notify();
            }));
        // Fallback drop target for the whole strip: a drop released over the
        // make-way gap (a margin, outside every tab's hitbox) still lands on
        // the tracked insertion position instead of silently ending the drag.
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
                    let width = f32::from(bounds.size.width);

                    if measured_width.get() != width {
                        measured_width.set(width);
                        // `Window::refresh` is a no-op mid-draw, so the redraw
                        // is requested through the shell entity instead.
                        measured_shell.update(cx, |_, cx| cx.notify());
                    }
                })
            })
            .on_drop(cx.listener(|this, drag: &TabDrag, window, cx| {
                if let Some(to) = this.tab_strip.drag_over.take() {
                    this.workspaces.active_tabs_mut().reorder(drag.from, to);

                    this.focus_active(window, cx);
                    this.sync_session_memory(cx);
                }
                cx.notify();
            }))
            .child(bar)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_bar_stops_at_rounded_tab_edges() {
        let tab_width = px(150.0);
        let bar_width = progress_bar_width(tab_width);

        assert_eq!(bar_width, px(134.0));
        assert_eq!(tab_width - UI_RADIUS - bar_width, UI_RADIUS);
    }

    /// Auto Size holds the configured width while the row has room, shares the
    /// row once it does not, and stops at the width that still shows the
    /// leading icon. It also has to survive a strip that has not been measured
    /// yet, which is every tab strip on its first frame.
    #[test]
    fn auto_size_shares_the_strip_between_tabs() {
        let configured = 120.0;
        let width = |strip: f32, count: usize| auto_tab_width(strip, count, configured);

        // Room to spare: no reason to shrink below what the user asked for.
        assert_eq!(width(1200.0, 2), configured);

        // Crowded: the tabs split what is left after the new-tab button, the
        // bar padding, and one gap each.
        let crowded = width(800.0, 8);
        assert!(
            crowded < configured,
            "{crowded} should be under {configured}"
        );
        assert!(crowded > MIN_AUTO_TAB_WIDTH);
        assert!(
            (crowded * 8.0 + TAB_GAP * 8.0 + TAB_BAR_PADDING + NEW_TAB_BUTTON_WIDTH - 800.0).abs()
                < 0.001,
            "the tabs and their gaps should consume the strip exactly",
        );

        // Past the floor the row overflows instead, and the strip scrolls.
        assert_eq!(width(800.0, 40), MIN_AUTO_TAB_WIDTH);

        // Before the first layout, and with nothing to lay out.
        assert_eq!(width(0.0, 8), configured);
        assert_eq!(width(1200.0, 0), configured);

        // A configured width under the floor is still an upper bound; Auto Size
        // never widens a tab past the setting.
        assert_eq!(auto_tab_width(800.0, 40, 30.0), 30.0);
    }

    /// The close control is the one thing a tab must never lose: the title
    /// goes first, then the leading icon gives up its slot on hover.
    #[test]
    fn a_shrinking_tab_gives_up_the_title_first() {
        assert_eq!(tab_density(120.0), TabDensity::Full);
        assert_eq!(tab_density(FULL_TAB_WIDTH), TabDensity::Full);
        assert_eq!(tab_density(FULL_TAB_WIDTH - 1.0), TabDensity::Compact);
        assert_eq!(tab_density(COMPACT_TAB_WIDTH), TabDensity::Compact);
        assert_eq!(tab_density(COMPACT_TAB_WIDTH - 1.0), TabDensity::IconOnly);

        // Every width Auto Size can produce still has a glyph slot for the
        // close control to take over.
        assert!(MIN_AUTO_TAB_WIDTH < COMPACT_TAB_WIDTH);
        assert_eq!(tab_density(MIN_AUTO_TAB_WIDTH), TabDensity::IconOnly);
    }

    #[test]
    fn busy_indicator_takes_precedence_over_ready() {
        assert_eq!(
            agent_tab_indicator(true, true),
            Some(AgentTabIndicator::Busy)
        );
        assert_eq!(
            agent_tab_indicator(false, true),
            Some(AgentTabIndicator::Ready)
        );
        assert_eq!(agent_tab_indicator(false, false), None);
    }
}
