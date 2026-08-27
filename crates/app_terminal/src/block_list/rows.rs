use crate::block_list::chrome::{item_accent, item_header};
use crate::block_list::selection::{expand_wide_span, selected_span};
use crate::block_list::*;

/// Builds one display line from an engine row visit (frozen-block row or
/// active-grid history row): every column contributes a char (gaps become
/// NBSP), spacer cells are dropped. Display conventions (wide placeholder,
/// run merging) come from the shared `LineBuilder`, so frozen rows shape and
/// paint exactly like live ones.
#[derive(Default)]
pub(crate) struct EngineRowBuilder {
    line: LineBuilder,
    col: u16,
}

impl EngineRowBuilder {
    pub(crate) fn push(
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

    pub(crate) fn finish(self) -> TerminalLine {
        self.line.finish()
    }
}

/// Chrome inputs of a block item, cloneable out of the store lock —
/// prepaint acquires the engine `BlockRef` afterwards, and the store and
/// engine locks must never nest (surface lock discipline).
#[derive(Clone)]
pub(crate) struct HandleItemInfo {
    pub handle: BlockHandle,
    /// Cached engine row count — the layout height source.
    pub rows: usize,
    pub accent: u32,
    pub header: Option<String>,
}

pub(crate) fn handle_item_info(item: &BlockItem) -> Option<HandleItemInfo> {
    let handle = item.handle()?;

    Some(HandleItemInfo {
        handle,
        rows: item.engine_rows(),
        accent: item_accent(&item.meta),
        header: item_header(&item.meta),
    })
}

/// The frozen view of an engine-block item: physical rows read through the
/// acquired [`BlockRef`], only for the visible range. Row `r`
/// keeps its item-local y even when earlier rows are skipped, so geometry
/// matches `item_px` exactly. `block = None` (stale handle / mid-reflow)
/// renders chrome at the cached height with no rows — content returns next
/// frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn frozen_block_view(
    block: Option<(&BlockRef, &Palette)>,
    info: &HandleItemInfo,
    item_idx: usize,
    visible: ops::Range<usize>,
    cell_h: f32,
    pad_rows: f32,
    selection: Option<(FrozenPoint, FrozenPoint)>,
    selected_item: Option<usize>,
) -> FrozenView {
    let default_fg = theme_default_foreground();
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

    let Some((block, palette)) = block else {
        return view;
    };

    let handle = block.handle();
    let cols = u32::from(block.cols());

    // The snapshot is the reading truth; the cached row count is the layout
    // truth. Read only rows both agree on (a lagging sync converges next
    // frame).
    let read_rows = rows.min(block.row_count());

    for row in visible.start..visible.end.min(read_rows) {
        let mut builder = EngineRowBuilder::default();
        let ok = block
            .read_row_visit(row, palette, |x, t, w, s| {
                builder.push(x, t, w, &s, default_fg)
            })
            .ok()
            .flatten()
            .is_some();

        if !ok {
            break;
        }

        let line = builder.finish();
        let selected =
            selected_span(selection, item_idx, row, cols).map(|span| expand_wide_span(&line, span));

        view.rows.push(FrozenRow {
            y: pad + row as f32 * cell_h,
            line,
            item: item_idx,
            row,
            cell_count: cols,
            selected,
            shape_key: Some(block_row_shape_key(handle, row)),
        });
    }
    view
}

/// Shaped-line cache key for an engine-block row: `(block_id, generation,
/// row)`. Content is immutable per generation, so the layout
/// caches across frames without hashing the row text.
fn block_row_shape_key(handle: BlockHandle, row: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = collections::hash_map::DefaultHasher::new();

    (handle.id, handle.generation, row).hash(&mut hasher);

    hasher.finish()
}

/// The live item's scrolled-up history: active-grid scrollback rows read as
/// physical lines rendered above the live grid. Rows carry
/// an out-of-band item index that the hit map converts back to their absolute
/// SCREEN row; selection remains owned by the engine rather than BlockStore.
pub(crate) fn live_history_view(
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
