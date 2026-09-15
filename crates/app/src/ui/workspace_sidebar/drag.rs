use gpui::prelude::*;
use gpui::{Context, Render, SharedString, Window, div, px};
use gpui_component::{ActiveTheme as _, h_flex};
use nmt_agent::AgentRuntimeStatus;
use nmt_config::appearance::TabBarStyle;

use crate::ui::workspace_sidebar::WORKSPACE_NAME_INSET;
use crate::ui::workspace_sidebar::status::WorkspaceStatus;
use crate::ui::{AppSettings, UI_RADIUS};
use crate::workspace::TerminalActivity;

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
        let vertical_tabs =
            cx.global::<AppSettings>().config().appearance.tab_bar_style == TabBarStyle::Vertical;

        // Dropped along with the lane on the row itself, so the ghost keeps
        // its name on the same leading edge as the list it came out of.
        let indicator = (!vertical_tabs).then(|| {
            WorkspaceStatus {
                agent: self.agent_status,
                terminal: self.terminal_activity,
            }
            .column("workspace-drag-status", "workspace-drag-busy", cx)
        });

        let background = cx
            .theme()
            .background
            .blend(cx.theme().sidebar)
            .blend(cx.theme().sidebar_accent);

        h_flex()
            .w(px(self.width))
            .pl(px(WORKSPACE_NAME_INSET))
            .pr_2()
            .py_1()
            .gap_2()
            .items_center()
            .rounded(UI_RADIUS)
            .overflow_hidden()
            .bg(background)
            .text_color(cx.theme().sidebar_accent_foreground)
            // Laid out like the row it was lifted from, so the ghost stays the
            // same height as the gap it will drop into.
            .child(
                h_flex()
                    .flex_1()
                    .gap_1p5()
                    .overflow_hidden()
                    .items_baseline()
                    .child(
                        div()
                            .min_w_0()
                            .text_left()
                            .text_sm()
                            .truncate()
                            .child(self.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_left()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().sidebar_accent_foreground.opacity(0.6))
                            .child(self.cwd.clone()),
                    ),
            )
            .children(indicator)
    }
}
