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

struct WorkspaceButtonProbe;

impl gpui::Render for WorkspaceButtonProbe {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement as _, ParentElement as _, Styled as _, div, px};

        use crate::ui::workspace_sidebar::WORKSPACE_NAME_TEXT;
        use crate::ui::workspace_sidebar::rows::workspace_row_button;

        div().size_full().child(
            div()
                .w(px(300.))
                .child(
                    workspace_row_button("idle-workspace", cx).child(
                        div()
                            .text_size(px(WORKSPACE_NAME_TEXT))
                            .truncate()
                            .child("mipmap gyjp 工作区")
                            .debug_selector(|| "workspace-name".into()),
                    ),
                )
                .debug_selector(|| "workspace-row".into()),
        )
    }
}

#[gpui::test]
fn idle_workspace_button_renders_and_accepts_hover(cx: &mut gpui::TestAppContext) {
    use gpui::{Modifiers, VisualTestContext, point, px};
    cx.update(gpui_component::init);
    let window = cx.add_window(|_, _| WorkspaceButtonProbe);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    cx.refresh().unwrap();
    let bounds = cx
        .debug_bounds("workspace-row")
        .expect("workspace row was not painted");
    assert_eq!(bounds.size.width, px(300.));
    let name = cx
        .debug_bounds("workspace-name")
        .expect("workspace name was not painted");
    assert!(
        name.size.height >= px(17.),
        "the clipped text box needs room for descenders: {name:?}"
    );
    assert!(bounds.top() < name.top() && bounds.bottom() > name.bottom());
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    cx.refresh().unwrap();
    cx.simulate_mouse_move(point(px(350.), px(100.)), None, Modifiers::default());
    cx.refresh().unwrap();
    assert_eq!(cx.debug_bounds("workspace-row"), Some(bounds));
}
