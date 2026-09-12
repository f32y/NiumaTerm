use crate::terminal_tab::pane_model::list_mirror::ListOp;

pub(in crate::terminal_tab) struct ListPlan {
    pub ops: Vec<ListOp>,
    pub history_rows: u64,
    pub live_index: usize,
    pub cols: u32,
}
