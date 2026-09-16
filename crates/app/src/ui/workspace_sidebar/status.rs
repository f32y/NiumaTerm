#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;

use std::borrow::Cow;

use gpui::prelude::*;
use gpui::{AnyElement, App, Div, ElementId, Stateful, px};
use gpui_component::v_flex;
use nmt_agent::AgentRuntimeStatus;
use rust_i18n::t;

use crate::terminal_tab::terminal_status::{terminal_dot, terminal_presentation};
use crate::ui::composition::{StatusMark, StatusMarkTone};
use crate::workspace::TerminalActivity;

/// What a workspace is doing, as the sidebar's status column reports it: its
/// agents' state above its terminals'.
#[derive(Clone, Copy)]
pub(super) struct WorkspaceStatus {
    pub(super) agent: AgentRuntimeStatus,
    pub(super) terminal: TerminalActivity,
}

/// Diameter of a status dot in the sidebar column. Smaller than the agent
/// spinner's `size_3`, so a stacked pair reads as a spinner with a mark under
/// it rather than as two equal glyphs.
const STATUS_DOT: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentVisual {
    Running,
    NeedsInput,
}

impl WorkspaceStatus {
    /// The column's accessible label, naming both halves when both show.
    pub(super) fn label(self) -> String {
        let agent = agent_presentation(self.agent);
        let terminal = terminal_presentation(self.terminal);

        status_column_label(
            agent.as_ref().map(|(_, label)| label.as_ref()),
            terminal.as_ref().map(|(_, label)| label.as_ref()),
        )
    }

    /// The column itself. `busy_id` identifies the agent's mark, which
    /// animates while the agent runs.
    pub(super) fn column(
        self,
        id: impl Into<ElementId>,
        busy_id: impl Into<ElementId>,
        cx: &App,
    ) -> Stateful<Div> {
        let (glyphs, label) = status_glyphs(self.agent, self.terminal, busy_id, cx);

        v_flex()
            .id(id)
            // The column's width is fixed so an idle workspace can suppress
            // its glyphs without shifting its name relative to active
            // neighbours; the height follows its contents so a stacked pair
            // centers as a group and a lone glyph centers on its own.
            .w_4()
            .flex_none()
            .gap_0p5()
            .items_center()
            .justify_center()
            .aria_label(label)
            .children(glyphs)
    }
}

/// The agent half of the status column, absent while the agent is idle.
fn agent_presentation(status: AgentRuntimeStatus) -> Option<(AgentVisual, Cow<'static, str>)> {
    match status {
        AgentRuntimeStatus::Running => {
            Some((AgentVisual::Running, t!("sidebar-workspace-status-running")))
        }
        AgentRuntimeStatus::NeedsInput => Some((
            AgentVisual::NeedsInput,
            t!("sidebar-workspace-status-needs-input"),
        )),
        AgentRuntimeStatus::Idle => None,
    }
}

/// One accessible label for whatever the column holds. The two halves report
/// independent things, so both are named when both are showing.
fn status_column_label(agent: Option<&str>, terminal: Option<&str>) -> String {
    match (agent, terminal) {
        (Some(agent), Some(terminal)) => t!(
            "sidebar-workspace-status-pair",
            agent = agent,
            terminal = terminal
        )
        .into_owned(),
        (Some(label), None) | (None, Some(label)) => label.to_string(),
        (None, None) => t!("sidebar-workspace-status-idle").to_string(),
    }
}

/// Glyphs for the status column, agent above terminal. The caller stacks them;
/// with one glyph the stack collapses to a centered single mark.
fn status_glyphs(
    status: AgentRuntimeStatus,
    terminal: TerminalActivity,
    busy_id: impl Into<ElementId>,
    cx: &App,
) -> (Vec<AnyElement>, String) {
    let agent = agent_presentation(status);
    let terminal = terminal_presentation(terminal);

    let label = status_column_label(
        agent.as_ref().map(|(_, label)| label.as_ref()),
        terminal.as_ref().map(|(_, label)| label.as_ref()),
    );

    let busy_id = busy_id.into();

    let glyphs = agent
        .map(|(visual, label)| match visual {
            AgentVisual::Running => StatusMark::busy(busy_id).into_any_element(),
            // Same success color the terminal mark uses when a command
            // finishes: both say the tab has stopped working and is waiting on
            // the user.
            AgentVisual::NeedsInput => {
                StatusMark::new(busy_id, StatusMarkTone::Success, px(STATUS_DOT))
                    .label(label)
                    .into_any_element()
            }
        })
        .into_iter()
        .chain(terminal.map(|(visual, _)| terminal_dot(visual, STATUS_DOT, cx)))
        .collect();

    (glyphs, label)
}
