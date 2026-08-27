use crate::terminal_view::*;
use crate::{block_list, frame, graphics};

pub(super) fn shape_frame(
    bounds: Bounds<Pixels>,
    frame: &TerminalFrame,
    cell: metrics::CellMetrics,
    window: &mut Window,
) -> Vec<ShapedLine> {
    let row_count =
        ((bounds.size.height.as_f32() / cell.height_px).ceil() as usize).min(frame.lines().len());

    shape_lines(
        frame
            .lines()
            .iter()
            .take(row_count)
            .map(|line| (line.text_hash(), line)),
        cell.width_px,
        window,
    )
}

pub(super) fn paint_frame(
    bounds: Bounds<Pixels>,
    frame: &TerminalFrame,
    lines: &[ShapedLine],
    cell: metrics::CellMetrics,
    offsets: &[f32],
    window: &mut Window,
    cx: &mut App,
) {
    use crate::frame::ZLayer;

    // Kitty images below cell backgrounds (z < i32::MIN/2).
    paint_frame_images(
        bounds,
        frame,
        ZLayer::BelowBackground,
        cell,
        offsets,
        window,
    );

    for (row, line) in frame.lines().iter().take(lines.len()).enumerate() {
        paint_line_backgrounds_at(
            bounds,
            line,
            row as f32 * cell.height_px + row_y_offset(offsets, row),
            cell.width_px,
            cell.height_px,
            window,
        );
    }

    // Kitty images above backgrounds, below cursor/text (i32::MIN/2 <= z < 0).
    paint_frame_images(bounds, frame, ZLayer::BelowText, cell, offsets, window);
    paint_cursor(bounds, frame.cursor(), cell, offsets, window);

    paint_glyph_rows(
        bounds,
        lines.iter().enumerate().map(|(row, line)| {
            (
                row as f32 * cell.height_px + row_y_offset(offsets, row),
                line,
            )
        }),
        cell.height_px,
        window,
        cx,
    );

    // Kitty images above cursor/text (z >= 0).
    paint_frame_images(bounds, frame, ZLayer::AboveText, cell, offsets, window);
}

/// Paint the frame's Kitty images whose z-index falls in `layer`, in engine order (no
/// per-paint sort or descriptor allocation). Each image's full texture is painted into
/// the source-expanded bounds and clipped to its destination by a content mask, so a
/// source crop needs no CPU cropping. A painted generation is marked
/// uploaded so its atlas tile is released once its last reference drops.
fn paint_frame_images(
    bounds: Bounds<Pixels>,
    frame: &TerminalFrame,
    layer: frame::ZLayer,
    cell: metrics::CellMetrics,
    offsets: &[f32],
    window: &mut Window,
) {
    let images = frame.images();

    if images.is_empty() {
        return; // no graphics: zero work
    }

    for img in images {
        if img.z_layer() != layer {
            continue;
        }

        let top = img.top_row();
        let row_offset = if top >= 0 {
            row_y_offset(offsets, top as usize)
        } else {
            0.0
        };

        let Some((dest, source)) = img.destination(
            cell.width_px,
            cell.height_px,
            f32::from(bounds.left()),
            f32::from(bounds.top()),
            row_offset,
        ) else {
            continue;
        };

        paint_generation(window, dest, source, &img.generation);
    }
}

/// Paint a block-list item's frozen Kitty image slices whose z-layer is on
/// the requested side of the frozen text: `above_text == false` paints the below-text
/// slices (before `paint_frozen`), `true` the above-text slices (after). Uses the same
/// source-crop primitive as live images; clips to each slice's destination cell rect.
pub(super) fn paint_frozen_images(
    bounds: Bounds<Pixels>,
    view: &block_list::FrozenView,
    cell: metrics::CellMetrics,
    window: &mut Window,
    above_text: bool,
) {
    if view.images.is_empty() {
        return;
    }

    for img in &view.images {
        if (img.z >= 0) != above_text {
            continue;
        }

        let dest = [
            f32::from(bounds.left()) + img.col as f32 * cell.width_px,
            f32::from(bounds.top()) + img.y,
            img.width as f32 * cell.width_px,
            cell.height_px,
        ];

        paint_generation(window, dest, img.source, &img.generation);
    }
}

/// Paint one image generation's `source` crop into `dest` and mark it
/// uploaded (its atlas tile releases with the last reference) — the shared
/// tail of live-frame and frozen image painting. Degenerate crops are
/// skipped.
fn paint_generation(
    window: &mut Window,
    dest: [f32; 4],
    source: [f32; 4],
    generation: &graphics::ImageGeneration,
) {
    let Some(full) = graphics::expanded_full_bounds(dest, source) else {
        return;
    };

    paint_image_clipped(window, dest, full, generation.image().clone());

    generation.mark_uploaded();
}

/// Paint `image`'s full texture into `full` bounds, clipped to `dest` — the source-crop
/// primitive. GPUI intersects the mask with the element's existing overflow
/// mask, so viewport clipping is automatic.
fn paint_image_clipped(
    window: &mut Window,
    dest: [f32; 4],
    full: [f32; 4],
    image: sync::Arc<RenderImage>,
) {
    let to_bounds = |b: [f32; 4]| Bounds {
        origin: point(px(b[0]), px(b[1])),
        size: size(px(b[2]), px(b[3])),
    };

    let mask = ContentMask {
        bounds: to_bounds(dest),
    };

    window.with_content_mask(Some(mask), |w| {
        let _ = w.paint_image(to_bounds(full), Corners::default(), image, 0, false);
    });
}

fn paint_cursor(
    bounds: Bounds<Pixels>,
    cursor: Option<TerminalCursor>,
    cell: metrics::CellMetrics,
    offsets: &[f32],
    window: &mut Window,
) {
    let Some(cursor) = cursor else {
        return;
    };

    let y_offset = row_y_offset(offsets, cursor.row as usize);

    let Some(bounds) = cursor_bounds(bounds, cursor, cell, y_offset) else {
        return;
    };

    window.paint_quad(fill(bounds, rgb(cursor.color.rgb_u32())));
}

pub(crate) fn cursor_bounds(
    bounds: Bounds<Pixels>,
    cursor: TerminalCursor,
    cell: metrics::CellMetrics,
    y_offset: f32,
) -> Option<Bounds<Pixels>> {
    let x = bounds.left() + px(cursor.col as f32 * cell.width_px);
    let y = bounds.top() + px(cursor.row as f32 * cell.height_px + y_offset);
    let thickness = px((cell.width_px.min(cell.height_px) / 8.0)
        .round()
        .clamp(1.0, 2.0));

    Some(match cursor.shape {
        CursorShape::Block => Bounds::new(point(x, y), size(px(cell.width_px), px(cell.height_px))),
        CursorShape::Beam => Bounds::new(point(x, y), size(thickness, px(cell.height_px))),
        CursorShape::Underline => Bounds::new(
            point(x, y + px(cell.height_px) - thickness),
            size(px(cell.width_px), thickness),
        ),
        CursorShape::Hidden => return None,
    })
}
