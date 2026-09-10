use crate::block_list::*;
use crate::metrics::CellMetrics;

/// Shape the visible frozen rows. Block rows cache by `(block_id,
/// generation, row)`; live-history rows hash their text.
pub(crate) fn shape_frozen_rows(
    rows: &[FrozenRow],
    cell_w: f32,
    window: &mut Window,
) -> Vec<ShapedLine> {
    shape_lines(
        rows.iter().map(|row| {
            (
                row.shape_key.unwrap_or_else(|| row.line.text_hash()),
                &row.line,
            )
        }),
        cell_w,
        window,
    )
}

/// Paint separators + frozen rows (backgrounds then glyphs).
pub(crate) fn paint_frozen(
    bounds: Bounds<Pixels>,
    view: &FrozenView,
    shaped: &[ShapedLine],
    cell: CellMetrics,
    window: &mut Window,
    cx: &mut App,
) {
    for row in &view.rows {
        paint_line_backgrounds_at(bounds, &row.line, row.y, cell, window);
    }

    // Selection tint under the glyphs (over the cell backgrounds).
    let selection_bg = theme_selection_background();

    for row in &view.rows {
        let Some((start, end)) = row.selected else {
            continue;
        };

        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() + px(start as f32 * cell.width_px),
                    bounds.top() + px(row.y),
                ),
                size(px((end - start) as f32 * cell.width_px), px(cell.height_px)),
            ),
            rgb(selection_bg.rgb_u32()),
        ));
    }

    paint_glyph_rows(
        bounds,
        view.rows
            .iter()
            .zip(shaped)
            .map(|(row, line)| (row.y, line)),
        cell.height_px,
        window,
        cx,
    );
}
