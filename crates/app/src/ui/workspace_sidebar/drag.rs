use gpui::prelude::*;
use gpui::{Context, Render, SharedString, Window, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};
use nmt_agent_utils::AgentRuntimeStatus;
use nmt_config::appearance::TabBarStyle;

use crate::tabs::TabId;
use crate::ui::workspace_sidebar::{TAB_ROW_HEIGHT, workspace_status_glyphs};
use crate::ui::{AppSettings, UI_RADIUS};
use crate::workspace::TerminalActivity;

/// A tab row picked up for reordering. The workspace id prevents a row from
/// being reordered into a different workspace's tab manager.
pub(super) struct SidebarTabDrag {
    pub(super) workspace: usize,
    pub(super) from: usize,
    pub(super) tab: TabId,
}

pub(super) struct SidebarTabDragPreview {
    pub(super) label: SharedString,
    pub(super) width: f32,
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

pub(super) struct WorkspaceDrag {
    pub(super) from: usize,
}

pub(super) struct WorkspaceDragPreview {
    pub(super) name: SharedString,
    pub(super) cwd: SharedString,
    pub(super) agent_status: AgentRuntimeStatus,
    pub(super) terminal_activity: TerminalActivity,
    pub(super) width: f32,
}

impl Render for WorkspaceDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (glyphs, status_label) = workspace_status_glyphs(
            self.agent_status,
            self.terminal_activity,
            "workspace-drag-busy",
            cx,
        );
        let vertical_tabs = cx.global::<AppSettings>().tab_bar_style == TabBarStyle::Vertical;
        let indicator = v_flex()
            .id("workspace-drag-status")
            .w_4()
            .flex_none()
            .gap_0p5()
            .items_center()
            .justify_center()
            .when(!vertical_tabs, |this| {
                this.aria_label(status_label).children(glyphs)
            });
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
