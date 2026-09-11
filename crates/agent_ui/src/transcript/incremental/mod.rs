use std::mem;

use nmt_config::agent::CollapseRows;
use nmt_profiling::transcript::{Operation, Probe};

use crate::transcript::rows::TranscriptRow;
use crate::transcript::{Entry, RowSpec, TranscriptView};

#[derive(Default)]
pub(super) struct RowCache {
    /// Only the suffix starting at the earliest changed turn needs rebuilding.
    dirty_from: Option<usize>,

    /// Exclusive entry and row ends for each contiguous turn, excluding the
    /// live progress row. Both offsets stay valid throughout an unchanged prefix.
    turns: Vec<(usize, usize)>,

    mode: Option<CollapseRows>,
    specs: Vec<RowSpec>,
    pub(super) scratch_rows: Vec<TranscriptRow>,
    #[cfg(test)]
    pub(super) rebuilt_entries: usize,
}

impl RowCache {
    pub(super) fn invalidate(&mut self, index: usize) {
        self.dirty_from = Some(self.dirty_from.map_or(index, |old| old.min(index)));
    }
}

impl TranscriptView {
    pub(super) fn append_entry(&mut self, entry: Entry) {
        let _profile = Probe::start(Operation::AppendEntry);

        // The previous last turn may gain another entry, and its final row's
        // spacing depends on the first row appended below it.
        let index = self.content.append(entry);

        self.row_cache.invalidate(index.saturating_sub(1));
    }

    pub(super) fn invalidate_turn_rows(&mut self, turn: u64) {
        if let Some(index) = self
            .content
            .entries()
            .iter()
            .position(|entry| entry.turn == turn)
        {
            self.row_cache.invalidate(index);
        } else {
            self.row_cache.invalidate(self.content.entries().len());
        }
    }

    pub(super) fn refresh_rows(&mut self, collapse: CollapseRows) {
        #[cfg(test)]
        {
            self.row_cache.rebuilt_entries = 0;
        }

        if self.row_cache.mode != Some(collapse) {
            self.row_cache.mode = Some(collapse);
            self.row_cache.invalidate(0);
        }

        let Some(dirty) = self.row_cache.dirty_from.take() else {
            return;
        };

        let _profile = Probe::start(Operation::RowsRebuild);
        let items = self.content.entries();

        let keep = self
            .row_cache
            .turns
            .partition_point(|(end, _)| *end <= dirty);

        self.row_cache.turns.truncate(keep);

        let (mut start, mut row_start) = self.row_cache.turns.last().copied().unwrap_or_default();

        #[cfg(test)]
        {
            self.row_cache.rebuilt_entries = items.len() - start;
        }

        let mut specs = mem::take(&mut self.row_cache.specs);

        specs.clear();

        // Reconsider the preceding row's gap along with the changed suffix.
        if row_start > 0 {
            row_start -= 1;
            specs.push(self.rows[row_start].spec.clone());
        }

        while start < items.len() {
            let turn = items[start].turn;
            let mut end = start + 1;

            while end < items.len() && items[end].turn == turn {
                end += 1;
            }

            self.turn_specs(turn, start, end, collapse, &mut specs);
            self.row_cache.turns.push((end, row_start + specs.len()));
            start = end;
        }

        if self.live_turn.is_working() {
            specs.push(RowSpec::Working {
                compacting: self.live_turn.is_compacting(),
            });
        }

        self.sync_transcript_tail(row_start, &specs);
        specs.clear();
        self.row_cache.specs = specs;
    }
}

#[cfg(test)]
mod tests;
