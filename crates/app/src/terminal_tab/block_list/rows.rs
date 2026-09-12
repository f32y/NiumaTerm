use std::sync::Arc;
use std::{collections, iter, ops};

use nmt_terminal::block_store::BlockItem;
use nmt_terminal::ghostty::{BlockHandle, CellText, CellWide, SnapshotStyle, Underline};
use nmt_terminal::grid_emit::row_selection_for;
use nmt_terminal::selection::SelectionRange;
use nmt_terminal::session::BlockPoint as FrozenPoint;
use nmt_terminal::session::interaction::block_selection_span;
use nmt_terminal::session::page::{PageSource, RowPage};
use nmt_terminal::terminal::square::Wide;

use crate::terminal_tab::block_list::chrome::{
    DurationLabels, FrozenItemChrome, item_accent, item_header,
};
use crate::terminal_tab::block_list::selection::expand_wide_span;
use crate::terminal_tab::block_list::{FrozenRow, FrozenView};
use crate::terminal_tab::frame::{
    LineBuilder, StyleRun, TerminalCell, TerminalColor, TerminalLine,
};

/// Builds one display line from an engine row visit (frozen-block row or
/// active-grid history row): every column contributes a char (gaps become
/// NBSP), spacer cells are dropped. Display conventions (wide placeholder,
/// run merging) come from the shared `LineBuilder`, so frozen rows shape and
/// paint exactly like live ones.
#[derive(Default)]
pub(in crate::terminal_tab) struct EngineRowBuilder {
    line: LineBuilder,
    col: u16,
}

impl EngineRowBuilder {
    pub(in crate::terminal_tab) fn push(
        &mut self,
        x: u16,
        cell_text: CellText,
        wide: CellWide,
        style: &SnapshotStyle,
        default_fg: TerminalColor,
    ) {
        use nmt_terminal::ghostty::CellWide;

        match wide {
            CellWide::SpacerTail | CellWide::SpacerHead => return,
            CellWide::Narrow | CellWide::Wide => {}
        }

        let default_style = StyleRun {
            len: 0,
            fg: default_fg,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
        };

        while self.col < x {
            self.line
                .push_segment(iter::once('\u{00a0}'), default_style, false);

            self.col += 1;
        }

        let (fg, bg) = if style.inverse {
            (
                style.bg.unwrap_or(default_fg),
                Some(style.fg.unwrap_or(default_fg)),
            )
        } else {
            (style.fg.unwrap_or(default_fg), style.bg)
        };

        let is_wide = wide == CellWide::Wide;

        let display: String = if cell_text.is_empty() {
            "\u{00a0}".into()
        } else {
            cell_text.replace([' ', '\t'], "\u{00a0}")
        };

        self.line.push_segment(
            display.chars(),
            StyleRun {
                len: 0,
                fg,
                bold: style.bold,
                italic: style.italic,
                underline: style.underline != Underline::None,
                strikethrough: style.strikethrough,
            },
            is_wide,
        );

        self.line.push_cell(TerminalCell {
            col: x,
            ch: cell_text.chars().next().unwrap_or('\0'),
            style_id: 0,
            background: bg,
            wide: if is_wide { Wide::Wide } else { Wide::Narrow },
            extras: cell_text.chars().skip(1).collect(),
            has_cursor: false,
        });

        self.col = x + if is_wide { 2 } else { 1 };
    }
}

/// Metadata determines the item's height even while its visible pages are
/// still being materialized by the engine owner.
#[derive(Clone)]
pub(in crate::terminal_tab) struct HandleItemInfo {
    /// Cached engine row count — the layout height source.
    pub rows: usize,

    pub accent: u32,
    pub header: Option<String>,
}

pub(in crate::terminal_tab) fn handle_item_info(
    item: &BlockItem,
    labels: &DurationLabels,
) -> Option<HandleItemInfo> {
    item.handle()?;

    Some(HandleItemInfo {
        rows: item.engine_rows(),
        accent: item_accent(&item.meta),
        header: item_header(&item.meta, labels),
    })
}

/// Build visible rows from immutable pages. Missing pages keep their layout
/// space until the asynchronous read completes and wakes the pane.
#[allow(clippy::too_many_arguments)]
pub(in crate::terminal_tab) fn frozen_block_view(
    pages: &[Arc<RowPage>],
    info: &HandleItemInfo,
    item_idx: usize,
    visible: ops::Range<usize>,
    cell_h: f32,
    pad_rows: f32,
    selection: Option<(FrozenPoint, FrozenPoint)>,
    selected_item: Option<usize>,
    default_fg: TerminalColor,
) -> FrozenView {
    let selection = selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let rows = info.rows;
    let pad = pad_rows * cell_h;

    let mut view = FrozenView {
        rows: Vec::new(),
        items_chrome: Vec::new(),
        separators: Vec::new(),
        images: Vec::new(),
        active_top: (rows as f32 + 2.0 * pad_rows) * cell_h,
    };

    if rows == 0 {
        return view;
    }

    // Every block opens with a rule on its top edge; the neighbors' pad rows
    // give it a blank line on each side.
    view.separators.push(0.0);

    view.items_chrome.push(FrozenItemChrome {
        item: item_idx,
        top: 0.0,
        bottom: view.active_top,
        header_y: pad,
        accent: info.accent,
        header: info.header.clone(),
        selected: selected_item == Some(item_idx),
    });

    let Some(first) = pages.first() else {
        return view;
    };

    let PageSource::Block {
        id,
        generation,
        theme,
    } = first.source
    else {
        return view;
    };

    let handle = BlockHandle { id, generation };
    let cols: u32 = first.cols.into();

    // Cached item metadata determines layout; only completed page reads can
    // contribute text until the owner publishes the missing ranges.
    let read_rows = rows;

    for row in visible.start..visible.end.min(read_rows) {
        let mut builder = EngineRowBuilder::default();

        let Some(data) = pages.iter().find_map(|page| page.row(row)) else {
            continue;
        };

        for cell in &data.cells {
            builder.push(
                cell.x,
                cell.text.clone(),
                cell.wide,
                &cell.style,
                default_fg,
            );
        }

        let line: TerminalLine = builder.into();

        let selected = block_selection_span(selection, item_idx, row, cols)
            .map(|span| expand_wide_span(&line, span));

        view.rows.push(FrozenRow {
            y: pad + row as f32 * cell_h,
            line,
            item: item_idx,
            row,
            cell_count: cols,
            selected,
            shape_key: Some(block_row_shape_key(handle, theme, row)),
        });
    }

    view
}

/// Shaped-line cache key for an engine-block row: `(block_id, generation,
/// row)`. Content is immutable per generation, so the layout
/// caches across frames without hashing the row text.
fn block_row_shape_key(handle: BlockHandle, theme: u64, row: usize) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = collections::hash_map::DefaultHasher::new();

    (handle.id, handle.generation, theme, row).hash(&mut hasher);

    hasher.finish()
}

/// The live item's scrolled-up history: active-grid scrollback rows read as
/// physical lines rendered above the live grid. Rows carry
/// an out-of-band item index that the hit map converts back to their absolute
/// SCREEN row; selection remains in the pane session rather than BlockStore.
pub(in crate::terminal_tab) fn live_history_view(
    lines: Vec<(u64, TerminalLine)>,
    total_rows: u64,
    cols: u32,
    cell_h: f32,
    pad_rows: f32,
    selection: Option<SelectionRange>,
) -> FrozenView {
    let pad = pad_rows * cell_h;

    let mut view = FrozenView {
        rows: Vec::new(),
        items_chrome: Vec::new(),
        separators: vec![0.0],
        images: Vec::new(),
        active_top: pad + total_rows as f32 * cell_h,
    };

    for (row, line) in lines {
        let selected = usize::try_from(row)
            .ok()
            .filter(|row| *row <= i32::MAX as usize)
            .and_then(|row| row_selection_for(selection, row, cols as usize))
            .map(|span| (span.lo, span.hi.saturating_add(1)))
            .map(|span| expand_wide_span(&line, span));

        view.rows.push(FrozenRow {
            y: pad + row as f32 * cell_h,
            line,
            item: usize::MAX,
            row: row.min(usize::MAX as u64) as usize,
            cell_count: cols,
            selected,
            shape_key: None,
        });
    }

    view
}

impl From<EngineRowBuilder> for TerminalLine {
    fn from(value: EngineRowBuilder) -> Self {
        value.line.into()
    }
}
