use std::path::PathBuf;

use nmt_terminal::session::interaction::PendingCopy;

use crate::terminal_tab::pane_model::scroll::ScrollOutcome;

#[derive(Debug)]
pub(crate) enum KeyOutcome {
    Ignored,
    Written,
    CopyPending(PendingCopy),
    Scrolled(ScrollOutcome),
}

pub(crate) enum TextInput<'a> {
    Commit(&'a str),
    DropPaths(&'a [PathBuf]),
    RerunSelectedBlock,
}
