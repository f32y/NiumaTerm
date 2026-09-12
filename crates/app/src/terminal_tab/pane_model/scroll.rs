use crate::terminal_tab::pane_model::list_mirror::ListOp;

#[derive(Debug)]
pub(in crate::terminal_tab) enum ScrollOutcome {
    Ignored,
    GridRequested,
    List(ListOp),
}
