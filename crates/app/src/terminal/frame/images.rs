use std::collections;
use std::sync::{self, Arc};

use nmt_terminal::ansi::kitty_virtual::{self, IncompletePlacement, PLACEHOLDER, PlaceholderRun};
use nmt_terminal::ghostty::SnapshotPlacement;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::terminal::square::ContentTag;

use crate::terminal;

/// A paintable Kitty image in a frame. Metrics-independent: it retains the
/// shared image generation by `Arc` (no pixel copy) plus the geometry needed to place
/// it; final pixel geometry is computed at paint from the active cell metrics and grid
/// bounds. Ordinary and virtual placements normalize to this one descriptor.
#[derive(Clone)]
pub(crate) struct FrameImage {
    pub(crate) generation: Arc<terminal::graphics::ImageGeneration>,
    pub(crate) z: i32,
    pub(crate) kind: FrameImageKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FrameImageKind {
    /// Ordinary overlay placement: Ghostty viewport cell position + grid span + sub-
    /// cell offsets, with a normalized source rectangle into the full image.
    Ordinary {
        viewport_col: i32,
        viewport_row: i32,
        grid_cols: u32,
        grid_rows: u32,
        cell_x_offset: u32,
        cell_y_offset: u32,
        source: [f32; 4],
    },
    /// One row-run of a virtual (Unicode-placeholder) placement, resolved against its
    /// `(image_id, placement_id)` metadata. Final geometry uses `compute_run_geometry`
    /// at paint (aspect-fit needs real cell metrics).
    Virtual {
        run: PlaceholderRun,
        placement_cols: u32,
        placement_rows: u32,
        image_w: u32,
        image_h: u32,
        screen_line: usize,
        screen_col: usize,
    },
}

/// The three Kitty protocol paint layers. Preserved from the placement's
/// z-index; paint buckets by this and keeps engine order within a bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZLayer {
    /// `z < i32::MIN / 2`: below cell backgrounds.
    BelowBackground,
    /// `i32::MIN / 2 <= z < 0`: above backgrounds, below cursor/text.
    BelowText,
    /// `z >= 0`: above cursor/text.
    AboveText,
}

/// One shared empty `Arc<[FrameImage]>` for graphics-free frames — cloning it is a
/// refcount bump, so the common case allocates nothing.
pub(super) fn empty_images() -> Arc<[FrameImage]> {
    static EMPTY: sync::OnceLock<Arc<[FrameImage]>> = sync::OnceLock::new();

    EMPTY.get_or_init(|| Arc::from(Vec::new())).clone()
}

impl FrameImage {
    /// The image's top viewport row, for computing its row displacement (fixed-bottom
    /// / block-list) before geometry.
    pub(crate) fn top_row(&self) -> i32 {
        match self.kind {
            FrameImageKind::Ordinary { viewport_row, .. } => viewport_row,
            FrameImageKind::Virtual { screen_line, .. } => screen_line as i32,
        }
    }

    /// Pixel destination rectangle `[x, y, w, h]` and normalized source rectangle
    /// `[u0, v0, u1, v1]` for painting this image. `origin_x`/`origin_y` are
    /// the terminal grid's top-left; `row_offset` is the extra y displacement for this
    /// image's top row (`top_row`). Ordinary placements map viewport cells + sub-cell
    /// offsets directly; virtual runs go through `compute_run_geometry` (aspect-fit).
    /// Returns `None` for degenerate geometry (paint skips it).
    pub(crate) fn destination(
        &self,
        cell_w: f32,
        cell_h: f32,
        origin_x: f32,
        origin_y: f32,
        row_offset: f32,
    ) -> Option<([f32; 4], [f32; 4])> {
        match self.kind {
            FrameImageKind::Ordinary {
                viewport_col,
                viewport_row,
                grid_cols,
                grid_rows,
                cell_x_offset,
                cell_y_offset,
                source,
            } => {
                let dx = origin_x + viewport_col as f32 * cell_w + cell_x_offset as f32;
                let dy =
                    origin_y + viewport_row as f32 * cell_h + row_offset + cell_y_offset as f32;
                let dw = grid_cols as f32 * cell_w;
                let dh = grid_rows as f32 * cell_h;

                if dw <= 0.0 || dh <= 0.0 {
                    return None;
                }

                Some(([dx, dy, dw, dh], source))
            }
            FrameImageKind::Virtual {
                run,
                placement_cols,
                placement_rows,
                image_w,
                image_h,
                screen_line,
                screen_col,
            } => {
                // Fold this row's displacement into the origin and paint the single
                // row at screen line 0 of the adjusted origin.
                let oy = origin_y + screen_line as f32 * cell_h + row_offset;
                let g = kitty_virtual::compute_run_geometry(
                    &run,
                    placement_cols,
                    placement_rows,
                    image_w,
                    image_h,
                    cell_w,
                    cell_h,
                    origin_x,
                    oy,
                    0,
                    screen_col,
                )?;

                Some(([g.x, g.y, g.width, g.height], g.source_rect))
            }
        }
    }

    pub(crate) fn z_layer(&self) -> ZLayer {
        if self.z < i32::MIN / 2 {
            ZLayer::BelowBackground
        } else if self.z < 0 {
            ZLayer::BelowText
        } else {
            ZLayer::AboveText
        }
    }
}

/// Build the paintable image descriptors for a frame. Resolves ordinary and
/// virtual placements against the pre-cloned live generation map; a placement whose
/// image is not cached is skipped (a later update wakes a rebuild). Preserves engine
/// placement order, ordinary before virtual. Metrics-independent — no cell sizing here.
pub(crate) fn extract_frame_images(
    buf: &RenderBuffer,
    generations: &collections::HashMap<u32, Arc<terminal::graphics::ImageGeneration>>,
) -> Vec<FrameImage> {
    // No-graphics fast path: with no placements there is nothing to extract, and a
    // row flagged virtual can only resolve against a virtual placement — so skip the
    // per-row placeholder scan entirely (zero cost when Kitty graphics are unused).
    if buf.placements().is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();

    extract_ordinary_images(buf, generations, &mut out);
    extract_virtual_images(buf, generations, &mut out);

    out
}

fn image_pixel_size(generation: &terminal::graphics::ImageGeneration) -> Option<(u32, u32)> {
    let size = generation.image().size(0);
    let (w, h) = (size.width.0.max(0) as u32, size.height.0.max(0) as u32);

    (w > 0 && h > 0).then_some((w, h))
}

fn extract_ordinary_images(
    buf: &RenderBuffer,
    generations: &collections::HashMap<u32, Arc<terminal::graphics::ImageGeneration>>,
    out: &mut Vec<FrameImage>,
) {
    for placement in buf.placements().iter().filter(|p| !p.is_virtual) {
        let Some(generation) = generations.get(&placement.image_id) else {
            continue; // pixels not cached yet; skip until the update arrives
        };

        let Some((iw, ih)) = image_pixel_size(generation) else {
            continue;
        };

        out.push(FrameImage {
            generation: generation.clone(),
            z: placement.z,
            kind: FrameImageKind::Ordinary {
                viewport_col: placement.viewport_col,
                viewport_row: placement.viewport_row,
                grid_cols: placement.grid_cols,
                grid_rows: placement.grid_rows,
                cell_x_offset: placement.cell_x_offset,
                cell_y_offset: placement.cell_y_offset,
                source: normalized_source_rect(placement, iw as f32, ih as f32),
            },
        });
    }
}

/// Normalize a placement's pixel source rectangle into `[u0, v0, u1, v1]`. A zero-size
/// source (Ghostty reports no explicit crop) maps to the full image.
fn normalized_source_rect(placement: &SnapshotPlacement, image_w: f32, image_h: f32) -> [f32; 4] {
    if placement.source_width == 0 || placement.source_height == 0 {
        return [0.0, 0.0, 1.0, 1.0];
    }

    let x0 = placement.source_x as f32 / image_w;
    let y0 = placement.source_y as f32 / image_h;
    let x1 = (placement.source_x + placement.source_width) as f32 / image_w;
    let y1 = (placement.source_y + placement.source_height) as f32 / image_h;

    [x0, y0, x1.min(1.0), y1.min(1.0)]
}

fn extract_virtual_images(
    buf: &RenderBuffer,
    generations: &collections::HashMap<u32, Arc<terminal::graphics::ImageGeneration>>,
    out: &mut Vec<FrameImage>,
) {
    for row in 0..buf.rows() {
        // Fast path: rows without any placeholder cell are never scanned.
        if !buf.row_has_virtual_placeholder(row) {
            continue;
        }

        let mut current: Option<(IncompletePlacement, usize)> = None;

        for col in 0..buf.cols() {
            let cell = buf.cell(col, row);
            let is_placeholder =
                cell.content_tag() == ContentTag::Codepoint && cell.c() == PLACEHOLDER;

            if !is_placeholder {
                if let Some((run, start)) = current.take() {
                    push_virtual_run(buf, generations, run, start, row, out);
                }

                continue;
            }

            let style = buf.style(cell.style_id());
            let combining = cell
                .extras_id()
                .and_then(|id| buf.extras().get(&id))
                .map(|extras| extras.zerowidth.as_slice())
                .unwrap_or(&[]);

            let inc = IncompletePlacement::from_cell(style.fg, style.underline_color, combining);

            match &mut current {
                Some((cur, _)) if cur.can_append(&inc) => cur.append(),
                _ => {
                    if let Some((run, start)) = current.take() {
                        push_virtual_run(buf, generations, run, start, row, out);
                    }

                    current = Some((inc, col));
                }
            }
        }
        if let Some((run, start)) = current.take() {
            push_virtual_run(buf, generations, run, start, row, out);
        }
    }
}

/// Resolve a completed placeholder run against its `(image_id, placement_id)` virtual
/// placement metadata and cached image, appending a `Virtual` descriptor. Skipped
/// without drawing a marker if either the placement metadata or the image is missing.
fn push_virtual_run(
    buf: &RenderBuffer,
    generations: &collections::HashMap<u32, Arc<terminal::graphics::ImageGeneration>>,
    incomplete: IncompletePlacement,
    start_col: usize,
    row: usize,
    out: &mut Vec<FrameImage>,
) {
    let run = incomplete.complete();

    let Some(placement) = buf
        .placements()
        .iter()
        .find(|p| p.is_virtual && p.image_id == run.image_id && p.placement_id == run.placement_id)
    else {
        return; // no matching placement metadata
    };

    let Some(generation) = generations.get(&run.image_id) else {
        return; // image not cached
    };

    let Some((iw, ih)) = image_pixel_size(generation) else {
        return;
    };

    out.push(FrameImage {
        generation: generation.clone(),
        z: placement.z,
        kind: FrameImageKind::Virtual {
            run,
            placement_cols: placement.grid_cols,
            placement_rows: placement.grid_rows,
            image_w: iw,
            image_h: ih,
            screen_line: row,
            screen_col: start_col,
        },
    });
}
