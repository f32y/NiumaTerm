#[cfg(test)]
#[path = "measurement_tests.rs"]
mod tests;

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::agent_tab::transcript::rows::{RowSpec, TranscriptRow};

#[derive(Default)]
pub(super) struct Measurements {
    entries: HashMap<usize, Range<usize>>,
    row_entries: Vec<Option<MeasuredRow>>,
    pending: HashSet<usize>,
    blocks: HashMap<(usize, usize), usize>,
    pending_blocks: HashSet<(usize, usize)>,
}

struct MeasuredRow {
    entry: usize,
    block_offset: Option<usize>,
}

impl Measurements {
    pub(super) fn invalidate(&mut self, entry: usize) {
        self.pending.insert(entry);
    }

    pub(super) fn invalidate_block(&mut self, entry: usize, offset: usize) {
        self.pending_blocks.insert((entry, offset));
    }

    pub(super) fn sync_tail(&mut self, start: usize, rows: &[TranscriptRow]) {
        let start = if start <= self.row_entries.len() {
            start
        } else {
            0
        };

        for row in self.row_entries.drain(start..).flatten() {
            let entry = row.entry;

            if let Some(offset) = row.block_offset {
                self.blocks.remove(&(entry, offset));
            }

            if let Some(range) = self.entries.get_mut(&entry) {
                if range.start < start {
                    range.end = start;
                } else {
                    self.entries.remove(&entry);
                }
            }
        }

        for (offset, row) in rows.iter().skip(start).enumerate() {
            let index = start + offset;

            let entry = match row.spec {
                RowSpec::Entry { index, .. } | RowSpec::Work { index, .. } => Some(index),
                _ => None,
            };

            self.row_entries.push(entry.map(|entry| MeasuredRow {
                entry,
                block_offset: row.block.as_ref().map(|block| block.key.0),
            }));

            if let (Some(entry), Some(block)) = (entry, &row.block) {
                self.blocks.insert((entry, block.key.0), index);
            }

            if let Some(entry) = entry {
                self.entries
                    .entry(entry)
                    .and_modify(|range| range.end = index + 1)
                    .or_insert(index..index + 1);
            }
        }
    }

    pub(super) fn take_ranges(&mut self) -> Vec<Range<usize>> {
        let mut ranges = self
            .pending
            .drain()
            .filter_map(|entry| self.entries.get(&entry).cloned())
            .collect::<Vec<_>>();

        for (entry, offset) in self.pending_blocks.drain() {
            if let Some(row) = self.blocks.get(&(entry, offset)) {
                ranges.push(*row..*row + 1);
            } else if let Some(range) = self.entries.get(&entry) {
                ranges.push(range.clone());
            }
        }

        ranges.sort_unstable_by_key(|range| range.start);

        let mut merged: Vec<Range<usize>> = Vec::new();

        for range in ranges {
            if let Some(previous) = merged.last_mut()
                && previous.end >= range.start
            {
                previous.end = previous.end.max(range.end);
            } else {
                merged.push(range);
            }
        }

        merged
    }
}
