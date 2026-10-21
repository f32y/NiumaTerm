use std::ops::Range;

use gpui::SharedString;
use ropey::Rope;
use sum_tree::{Bias, ContextLessSummary, Dimension, Item, SumTree};

use crate::agent_tab::transcript::code::ansi::AnsiStyle;
use crate::agent_tab::transcript::code::prepared::transcript_segments;
#[cfg(test)]
use crate::agent_tab::transcript::code::streaming_tests::record_indexed;

#[derive(Clone)]
pub(super) struct CodeRow {
    pub(super) range: Range<usize>,
    pub(super) text: SharedString,
    ansi: Vec<(Range<usize>, AnsiStyle)>,
}

impl Item for CodeRow {
    type Summary = RowSummary;

    fn summary(&self, _: ()) -> RowSummary {
        RowSummary {
            count: 1,
            end: self.range.end,
            widest: 0,
            width: self.range.len(),
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct RowSummary {
    count: usize,
    end: usize,
    widest: usize,
    width: usize,
}

impl ContextLessSummary for RowSummary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, other: &Self) {
        if other.width >= self.width && other.count > 0 {
            self.widest = self.count + other.widest;
            self.width = other.width;
        }

        self.count += other.count;
        self.end = self.end.max(other.end);
    }
}

#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct RowCount(usize);

impl<'a> Dimension<'a, RowSummary> for RowCount {
    fn zero(_: ()) -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &'a RowSummary, _: ()) {
        self.0 += summary.count;
    }
}

#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct ByteEnd(usize);

impl<'a> Dimension<'a, RowSummary> for ByteEnd {
    fn zero(_: ()) -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &'a RowSummary, _: ()) {
        self.0 = self.0.max(summary.end);
    }
}

#[derive(Clone, Default)]
pub(super) struct CodeRows {
    tree: SumTree<CodeRow>,
}

impl CodeRows {
    pub(super) fn len(&self) -> usize {
        self.tree.summary().count
    }

    pub(super) fn widest(&self) -> usize {
        self.tree.summary().widest
    }

    pub(super) fn get(&self, index: usize) -> Option<&CodeRow> {
        let mut cursor = self.tree.cursor::<RowCount>(());

        cursor.seek(&RowCount(index), Bias::Right);

        cursor.item()
    }

    #[cfg(test)]
    pub(super) fn iter(&self) -> impl Iterator<Item = &CodeRow> {
        self.tree.iter()
    }

    pub(super) fn append(&mut self, text: &Rope, spans: Vec<(Range<usize>, AnsiStyle)>) {
        let (start, mut ansi) = self
            .tree
            .last()
            .map_or((0, Vec::new()), |row| (row.range.start, row.ansi.clone()));

        if self.len() > 0 {
            let prefix = self
                .tree
                .cursor::<RowCount>(())
                .slice(&RowCount(self.len() - 1), Bias::Right);

            self.tree = prefix;
        }

        ansi.extend(spans);

        // Only the final bounded segment can grow. Earlier rows and their
        // text stay shared with snapshots already used by the renderer.
        let tail = text.slice(start..).to_string();

        #[cfg(test)]
        record_indexed(tail.len());

        let mut span_index = 0;

        for local in transcript_segments(&tail) {
            let range = start + local.start..start + local.end;

            while ansi
                .get(span_index)
                .is_some_and(|(span, _)| span.end <= range.start)
            {
                span_index += 1;
            }

            let mut styles: Vec<(Range<usize>, AnsiStyle)> = Vec::new();

            for (span, style) in ansi[span_index..]
                .iter()
                .take_while(|(span, _)| span.start < range.end)
            {
                let clipped = span.start.max(range.start)..span.end.min(range.end);

                if let Some((previous, previous_style)) = styles.last_mut()
                    && previous.end == clipped.start
                    && previous_style == style
                {
                    previous.end = clipped.end;
                } else {
                    styles.push((clipped, *style));
                }
            }

            self.tree.push(
                CodeRow {
                    text: tail[local].to_owned().into(),
                    range,
                    ansi: styles,
                },
                (),
            );
        }
    }

    pub(super) fn ansi(&self, range: &Range<usize>) -> Vec<(Range<usize>, AnsiStyle)> {
        let mut cursor = self.tree.cursor::<ByteEnd>(());

        cursor.seek(&ByteEnd(range.start), Bias::Right);

        let mut spans = Vec::new();

        while let Some(row) = cursor.item() {
            if row.range.start >= range.end {
                break;
            }

            spans.extend(row.ansi.iter().filter_map(|(span, style)| {
                let clipped = span.start.max(range.start)..span.end.min(range.end);

                (!clipped.is_empty()).then_some((clipped, *style))
            }));

            cursor.next();
        }

        spans
    }
}
