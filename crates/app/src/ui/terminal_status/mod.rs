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
    Running,
    Succeeded,
    Failed,
}

/// The mark's meaning, absent while no command is running and none has
/// finished unseen.
pub(crate) fn terminal_presentation(
    terminal: TerminalActivity,
) -> Option<(TerminalVisual, &'static str)> {
    match terminal {
        TerminalActivity::Running => Some((
            TerminalVisual::Running,
            i18n("terminal-status-command-running"),
        )),
        TerminalActivity::Finished(CommandOutcome::Succeeded) => Some((
            TerminalVisual::Succeeded,
            i18n("terminal-status-command-succeeded"),
        )),
        TerminalActivity::Finished(CommandOutcome::Failed) => Some((
            TerminalVisual::Failed,
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
        TerminalVisual::Running => cx.theme().muted_foreground,
        TerminalVisual::Succeeded => cx.theme().success,
        TerminalVisual::Failed => cx.theme().danger,
    };

    div()
        .size(px(size))
        .rounded_full()
        .bg(color)
        .into_any_element()
}

#[cfg(test)]
mod tests;
