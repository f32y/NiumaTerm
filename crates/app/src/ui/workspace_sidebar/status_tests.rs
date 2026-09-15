use nmt_agent::AgentRuntimeStatus;

use crate::tabs::CommandOutcome;
use crate::ui::terminal_status::terminal_presentation;
use crate::ui::workspace_sidebar::status::{AgentVisual, agent_presentation, status_column_label};
use crate::workspace::TerminalActivity;

#[test]
fn an_idle_workspace_supplies_no_glyph_but_retains_semantics() {
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
        status_column_label(Some(&agent_label), Some(&terminal_label)),
        "Needs input, Command failed"
    );
}
