use crate::tabs::CommandOutcome;
use crate::ui::terminal_status::{TerminalVisual, terminal_presentation};
use crate::workspace::TerminalActivity;

#[test]
fn terminal_activity_grades_the_dot() {
    let visual = |terminal| terminal_presentation(terminal).map(|(visual, _)| visual);

    assert_eq!(visual(TerminalActivity::Idle), None);
    assert_eq!(
        visual(TerminalActivity::Running),
        Some(TerminalVisual::Running)
    );
    assert_eq!(
        visual(TerminalActivity::Finished(CommandOutcome::Succeeded)),
        Some(TerminalVisual::Succeeded)
    );
    assert_eq!(
        visual(TerminalActivity::Finished(CommandOutcome::Failed)),
        Some(TerminalVisual::Failed)
    );
}
