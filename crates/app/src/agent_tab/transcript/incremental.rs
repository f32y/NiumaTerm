#[cfg(test)]
#[path = "incremental_tests.rs"]
mod incremental_tests;

use nmt_config::agent::CollapseRows;

use crate::agent_tab::transcript::RowSpec;
use crate::agent_tab::transcript::rows::TranscriptRow;

#[derive(Default)]
pub(super) struct RowCache {
    /// Only the suffix starting at the earliest changed turn needs rebuilding.
    pub(super) dirty_from: Option<usize>,

    /// Exclusive entry and row ends for each contiguous turn, excluding the
    /// live progress row. Both offsets stay valid throughout an unchanged prefix.
    pub(super) turns: Vec<(usize, usize)>,

    pub(super) mode: Option<CollapseRows>,
    pub(super) specs: Vec<RowSpec>,
    pub(super) scratch_rows: Vec<TranscriptRow>,
    #[cfg(test)]
    pub(super) rebuilt_entries: usize,
}

impl RowCache {
    pub(super) fn invalidate(&mut self, index: usize) {
        self.dirty_from = Some(self.dirty_from.map_or(index, |old| old.min(index)));
    }
}
