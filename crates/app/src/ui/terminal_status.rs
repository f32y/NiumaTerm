//! The terminal-activity mark, shared by the workspace sidebar entry and the
//! tab it belongs to. Both surfaces grade the same state, so the color and
//! wording live here rather than being spelled out twice.

use gpui::prelude::*;
use gpui::{AnyElement, App, div, px};
use gpui_component::ActiveTheme;
use nmt_i18n::i18n;

use crate::tabs::CommandOutcome;
use crate::workspace::TerminalActivity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalVisual {
    CommandRunning,
    CommandSucceeded,
    CommandFailed,
}

/// The mark's meaning, absent while no command is running and none has
/// finished unseen.
pub(crate) fn terminal_presentation(
    terminal: TerminalActivity,
) -> Option<(TerminalVisual, &'static str)> {
    match terminal {
        TerminalActivity::Running => Some((
            TerminalVisual::CommandRunning,
            i18n("terminal-status-command-running"),
        )),
        TerminalActivity::Finished(CommandOutcome::Succeeded) => Some((
            TerminalVisual::CommandSucceeded,
            i18n("terminal-status-command-succeeded"),
        )),
        TerminalActivity::Finished(CommandOutcome::Failed) => Some((
            TerminalVisual::CommandFailed,
            i18n("terminal-status-command-failed"),
        )),
        TerminalActivity::Idle => None,
    }
}

/// The mark itself, at `size` pixels across.
pub(crate) fn terminal_dot(visual: TerminalVisual, size: f32, cx: &App) -> AnyElement {
    // A running command is ambient activity the user started themselves, so it
    // reads in the muted accent; its result then takes the colors the rest of
    // the app uses to grade an outcome.
    let color = match visual {
        TerminalVisual::CommandRunning => cx.theme().muted_foreground,
        TerminalVisual::CommandSucceeded => cx.theme().success,
        TerminalVisual::CommandFailed => cx.theme().danger,
    };

    div()
        .size(px(size))
        .rounded_full()
        .bg(color)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use crate::tabs::CommandOutcome;
    use crate::ui::terminal_status::{TerminalVisual, terminal_presentation};
    use crate::workspace::TerminalActivity;

    #[test]
    fn terminal_activity_grades_the_dot() {
        let visual = |terminal| terminal_presentation(terminal).map(|(visual, _)| visual);

        assert_eq!(visual(TerminalActivity::Idle), None);
        assert_eq!(
            visual(TerminalActivity::Running),
            Some(TerminalVisual::CommandRunning)
        );
        assert_eq!(
            visual(TerminalActivity::Finished(CommandOutcome::Succeeded)),
            Some(TerminalVisual::CommandSucceeded)
        );
        assert_eq!(
            visual(TerminalActivity::Finished(CommandOutcome::Failed)),
            Some(TerminalVisual::CommandFailed)
        );
    }
}
