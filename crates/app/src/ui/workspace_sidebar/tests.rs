use nmt_agent_utils::AgentRuntimeStatus;

use crate::tabs::CommandOutcome;
use crate::ui::terminal_status::terminal_presentation;
use crate::ui::workspace_sidebar::{
    AgentVisual, agent_presentation, status_column_label, tail_preserving_path,
    workspace_display_label,
};
use crate::workspace::TerminalActivity;

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
