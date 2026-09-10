use std::collections::HashMap;
use std::mem;

use nmt_config::agent::CollapseRows;
use nmt_profiling::transcript::{Operation, Probe};
use smallvec::SmallVec;

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

#[derive(Default)]
pub(super) struct ItemIndex(HashMap<String, SmallVec<[usize; 1]>>);

impl ItemIndex {
    pub(super) fn insert(&mut self, entry: &Entry, index: usize) {
        if let Some(id) = entry.item.id() {
            self.0.entry(id.to_owned()).or_default().push(index);
        }
    }

    pub(super) fn positions(&self, id: &str) -> &[usize] {
        self.0.get(id).map_or(&[], |positions| positions.as_slice())
    }

    pub(super) fn clear(&mut self) {
        self.0.clear();
    }
}

impl TranscriptView {
    pub(super) fn append_entry(&mut self, entry: Entry) {
        let _profile = Probe::start(Operation::AppendEntry);

        // The previous last turn may gain another entry, and its final row's
        // spacing depends on the first row appended below it.
        self.row_cache
            .invalidate(self.items.len().saturating_sub(1));
        self.item_index.insert(&entry, self.items.len());
        self.items.push(entry);
    }

    pub(super) fn invalidate_turn_rows(&mut self, turn: u64) {
        if let Some(index) = self.items.iter().position(|entry| entry.turn == turn) {
            self.row_cache.invalidate(index);
        } else {
            self.row_cache.invalidate(self.items.len());
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
        let keep = self
            .row_cache
            .turns
            .partition_point(|(end, _)| *end <= dirty);

        self.row_cache.turns.truncate(keep);

        let (mut start, mut row_start) = self.row_cache.turns.last().copied().unwrap_or_default();

        #[cfg(test)]
        {
            self.row_cache.rebuilt_entries = self.items.len() - start;
        }

        let mut specs = mem::take(&mut self.row_cache.specs);

        specs.clear();

        // Reconsider the preceding row's gap along with the changed suffix.
        if row_start > 0 {
            row_start -= 1;
            specs.push(self.rows[row_start].spec.clone());
        }

        while start < self.items.len() {
            let turn = self.items[start].turn;
            let mut end = start + 1;

            while end < self.items.len() && self.items[end].turn == turn {
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
