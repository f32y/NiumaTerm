use crate::terminal_tab::block_list::reconcile::shift_selected_item_for_eviction;

#[derive(Default)]
pub(in crate::terminal_tab) struct GutterSelection {
    selected: Option<usize>,
}

impl GutterSelection {
    pub(in crate::terminal_tab) fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub(in crate::terminal_tab) fn select(&mut self, item: usize) {
        self.selected = Some(item);
    }

    /// Drop the gutter selection, reporting whether one was showing.
    pub(in crate::terminal_tab) fn clear_selection(&mut self) -> bool {
        self.selected.take().is_some()
    }

    /// Follow the selected item through a store eviction, so the highlight
    /// stays on the same command rather than on the same index.
    pub(in crate::terminal_tab) fn shift_for_eviction(&mut self, evicted: usize, store_len: usize) {
        self.selected = shift_selected_item_for_eviction(self.selected, evicted, store_len);
    }
}
