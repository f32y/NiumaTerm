use crate::pane_model::list_mirror::ListOp;

#[derive(Debug)]
pub(crate) enum ScrollOutcome {
    Ignored,
    GridRequested,
    List(ListOp),
}
