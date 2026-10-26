//! What the main surface shows for the active tab: a terminal pane tree, an
//! Agent or team pane, the Git review, or the settings page.

use app::agent_tab::AgentPane;
use app::terminal_tab::view::TerminalPane;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Div, Entity, MouseDownEvent, StyleRefinement, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::resizable::{ResizablePanelGroup, resizable_panel};
use gpui_component::setting::SettingsView;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use rust_i18n::t;

use crate::tabs::TabId;
use crate::ui::main_view_background_opacity;
use crate::ui::pane_tree::{PaneId, PaneNode};
use crate::ui::shell::{AppWindow, TabSurface};

/// The active tab's pane tree as nested resizable groups. The main surface
/// owns the outer frame, so a single pane renders without another card.
///
/// Each pane is embedded as a cached view. The window re-renders from its
/// root whenever anything in the chrome changes, and a busy mark in the
/// sidebar or tab strip animates for as long as an agent works; without the
/// cache every one of those frames would also rebuild the transcript or the
/// terminal grid. A pane still re-renders when it, a view inside it, or an
/// entity or global it read is notified or changed.
///
/// `agent` is the Agent pane the surface holds, if any, and `settings` the
/// settings page to show when the surface is the settings entry.
pub(super) fn tab_surface_view(
    surface: &TabSurface,
    tab_id: TabId,
    agent: Option<Entity<AgentPane>>,
    settings: Option<Entity<SettingsView>>,
    cx: &mut Context<AppWindow>,
) -> AnyElement {
    match surface {
        TabSurface::Hibernated(_) => return hibernated_tab_view(tab_id, cx),
        TabSurface::Git(tab) => {
            return div()
                .size_full()
                .overflow_hidden()
                .child(tab.view.clone().cached(pane_style()))
                .into_any_element();
        }
        TabSurface::Team(pane) => {
            return div()
                .size_full()
                .overflow_hidden()
                .child(pane.clone().cached(pane_style()))
                .into_any_element();
        }
        TabSurface::TeamUnavailable { message, .. } => {
            return div()
                .size_full()
                .p_4()
                .child(message.clone())
                .into_any_element();
        }
        TabSurface::TeamDisabled(_) => {
            return div()
                .size_full()
                .p_4()
                .child(t!("team-disabled").into_owned())
                .into_any_element();
        }
        TabSurface::Orchestration(pane) => {
            return div()
                .size_full()
                .overflow_hidden()
                .child(pane.clone().cached(pane_style()))
                .into_any_element();
        }
        TabSurface::OrchestrationDisabled(_) => {
            return div()
                .size_full()
                .p_4()
                .child(t!("orchestration-disabled").into_owned())
                .into_any_element();
        }
        _ => {}
    }

    if surface.is_settings() {
        return div()
            .size_full()
            .overflow_hidden()
            // The Settings widget paints no fill of its own, so without
            // this the translucent surface card shows the window backdrop
            // through the page area while the sidebar, which paints its
            // own fill, stays opaque.
            .bg(cx
                .theme()
                .background
                .alpha(main_view_background_opacity(cx)))
            .children(settings)
            .into_any_element();
    }

    if let Some(agent) = agent {
        return div()
            .size_full()
            .overflow_hidden()
            .child(agent.cached(pane_style()))
            .into_any_element();
    }

    let tree = surface.live();

    let multi = !tree.tree().is_single_leaf();

    render_pane_node(tree.tree().root(), tree.tree().focused(), multi, cx)
}

fn hibernated_tab_view(tab_id: TabId, cx: &mut Context<AppWindow>) -> AnyElement {
    div()
        .id("hibernated-tab-overlay")
        .size_full()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .backdrop_blur(px(24.))
        .bg(cx.theme().background.opacity(0.45))
        .text_color(cx.theme().foreground)
        .child(
            v_flex()
                .max_w(px(480.))
                .gap_4()
                .items_center()
                .child(div().text_lg().child(t!("shell-hibernated-tab-title")))
                .child(
                    div()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("shell-hibernated-tab-description")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("wake-hibernated-tab")
                                .debug_selector(|| "wake-hibernated-tab".into())
                                .primary()
                                .label(t!("shell-hibernated-tab-wake"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.wake_tab(tab_id, window, cx);
                                })),
                        )
                        .child(
                            Button::new("close-hibernated-tab")
                                .debug_selector(|| "close-hibernated-tab".into())
                                .outline()
                                .label(t!("menu-close-tab"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.request_close_tab(tab_id, window, cx);
                                })),
                        ),
                ),
        )
        .into_any_element()
}

fn render_pane_node(
    node: &PaneNode<Entity<TerminalPane>>,
    focused: PaneId,
    multi: bool,
    cx: &mut Context<AppWindow>,
) -> AnyElement {
    match node {
        PaneNode::Leaf { id, pane, .. } => {
            let id = *id;

            div()
                .size_full()
                // Split leaves retain equal-width borders so focus changes
                // never shift layout; the parent surface clips their outer
                // edges and provides the single-pane frame.
                .when(multi, |this| {
                    this.border_1().border_color(if id == focused {
                        cx.theme().primary
                    } else {
                        cx.theme().border
                    })
                })
                .capture_any_mouse_down(cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.focus_pane(id, window, cx);
                }))
                .child(pane.clone().cached(pane_style()))
                .into_any_element()
        }
        PaneNode::Split {
            id,
            axis,
            children,
            state,
            ..
        } => {
            let mut group = ResizablePanelGroup::new(("pane-split", *id as usize))
                .axis(*axis)
                .with_state(state);

            for child in children {
                group = group
                    .child(resizable_panel().child(render_pane_node(child, focused, multi, cx)));
            }

            group.into_any_element()
        }
    }
}

/// A cached pane is laid out from this style instead of measured from its
/// contents, so it has to fill the slot its parent gives it.
fn pane_style() -> StyleRefinement {
    StyleRefinement::default().size_full()
}

/// Clip the active tab's content within the sidebar-colored backing surface.
///
/// Terminal, Agent, Team and Orchestration panes cover the whole card with
/// their own translucent fill, so the card leaves those surfaces unfilled. Two
/// stacked translucent layers would hide most of the window background image
/// behind the content area while the chrome shows it at the configured
/// strength.
pub(super) fn floating_surface_card(surface: &TabSurface, cx: &App) -> Div {
    let pane_fills_card = matches!(
        surface,
        TabSurface::Live(_)
            | TabSurface::Hibernated(_)
            | TabSurface::Agent(_)
            | TabSurface::Team(_)
            | TabSurface::Orchestration(_)
    );

    div()
        .size_full()
        .overflow_hidden()
        .when(!pane_fills_card, |card| card.bg(cx.theme().sidebar))
}

/// Borders overlay content so attached tab and navigation bounds share one origin.
pub(super) fn surface_border(cx: &App) -> Div {
    div()
        .absolute()
        .inset_0()
        .border_l_1()
        .border_t_1()
        .border_color(cx.theme().sidebar_border)
}
