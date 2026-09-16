//! What the main surface shows for the active tab: a terminal pane tree, an
//! Agent or team pane, the Git review, or the settings page.

use app::agent_tab::AgentPane;
use app::terminal_tab::view::TerminalPane;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Div, Entity, MouseDownEvent, div};
use gpui_component::ActiveTheme;
use gpui_component::resizable::{ResizablePanelGroup, resizable_panel};
use gpui_component::setting::SettingsView;
use rust_i18n::t;

use crate::pane_tree::{PaneId, PaneNode};
use crate::ui::main_view_background_opacity;
use crate::ui::shell::{AppWindow, TabSurface};

/// The active tab's pane tree as nested resizable groups. The main surface
/// owns the outer frame, so a single pane renders without another card.
///
/// `agent` is the Agent pane the surface holds, if any, and `settings` the
/// settings page to show when the surface is the settings entry.
pub(super) fn tab_surface_view(
    surface: &TabSurface,
    agent: Option<Entity<AgentPane>>,
    settings: Option<Entity<SettingsView>>,
    cx: &mut Context<AppWindow>,
) -> AnyElement {
    match surface {
        TabSurface::Git(tab) => {
            return div()
                .size_full()
                .overflow_hidden()
                .child(tab.view.clone())
                .into_any_element();
        }
        TabSurface::Team(pane) => {
            return div()
                .size_full()
                .overflow_hidden()
                .child(pane.clone())
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
        _ => {}
    }

    if surface.is_settings() {
        return div()
            .size_full()
            .overflow_hidden()
            // The Settings widget paints no fill of its own, so without
            // this the translucent surface card shows the window backdrop
            // through the page area while the sidebar, which carries an
            // explicit fill, stays opaque.
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
            .child(agent)
            .into_any_element();
    }

    let tree = surface.live();

    let multi = !tree.tree().is_single_leaf();

    render_pane_node(tree.tree().root(), tree.tree().focused(), multi, cx)
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
                .child(pane.clone())
                .into_any_element()
        }
        PaneNode::Split {
            id,
            axis,
            children,
            state,
            ..
        } => {
            let shell = cx.entity();

            let mut group = ResizablePanelGroup::new(("pane-split", *id as usize))
                .axis(*axis)
                .with_state(state)
                // Keep the in-memory session mirror's split ratios fresh
                // after divider drags (the quit hook reads it).
                .on_resize(move |_, _, cx| {
                    shell.update(cx, |this, cx| this.sync_session_memory(cx));
                });

            for child in children {
                group = group
                    .child(resizable_panel().child(render_pane_node(child, focused, multi, cx)));
            }

            group.into_any_element()
        }
    }
}

/// Clip terminal and agent content within the sidebar-colored backing surface.
pub(super) fn floating_surface_card(cx: &App) -> Div {
    div().size_full().overflow_hidden().bg(cx.theme().sidebar)
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
