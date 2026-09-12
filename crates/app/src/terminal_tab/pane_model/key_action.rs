use std::path::PathBuf;

use nmt_terminal::session::interaction::PendingCopy;

use crate::terminal_tab::pane_model::scroll::ScrollOutcome;

#[derive(Debug)]
pub(in crate::terminal_tab) enum KeyOutcome {
    Ignored,
    Written,
    CopyPending(PendingCopy),
    Scrolled(ScrollOutcome),
}

pub(in crate::terminal_tab) enum TextInput<'a> {
    Commit(&'a str),
    DropPaths(&'a [PathBuf]),
    RerunSelectedBlock,
}
