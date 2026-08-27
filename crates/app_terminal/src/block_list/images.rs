use crate::block_list::*;
use crate::graphics;

/// A frozen Kitty image band positioned inside one block-list item: one
/// cell-row-high slice of a placement, with the generation shared
/// by `Arc` from the store's lazy `(block_id, image_id)` cache.
pub(crate) struct FrozenImage {
    pub generation: sync::Arc<graphics::ImageGeneration>,
    pub z: i32,
    /// Element-local y of the row's top edge.
    pub y: f32,
    /// Column within the row, and cell width of the band.
    pub col: u32,
    pub width: u32,
    /// Normalized source rectangle `[u0, v0, u1, v1]` into the full image.
    pub source: [f32; 4],
}

/// Frozen Kitty images of an engine-block item, mapped to the visible row
/// range: each placement contributes one cell-row band per
/// visible row it spans, with the source rectangle subdivided vertically
/// (boundary-difference math — no cumulative rounding gaps). Placement
/// positions are block-relative rows straight from the engine; generations
/// come from the store's `(block_id, image_id)` lazy cache.
pub(crate) fn frozen_block_images(
    placements: &[PlacementScreenPos],
    generations: &collections::HashMap<u32, sync::Arc<graphics::ImageGeneration>>,
    visible: &ops::Range<usize>,
    cell_h: f32,
    pad_rows: f32,
) -> Vec<FrozenImage> {
    let pad = pad_rows * cell_h;
    let mut out = Vec::new();

    for p in placements {
        if p.grid_rows == 0 || p.grid_cols == 0 {
            continue;
        }

        let Some(generation) = generations.get(&p.image_id) else {
            continue; // pixels unavailable (evicted mid-read); retry next frame
        };

        let size = generation.image().size(0);
        let (iw, ih) = (size.width.0.max(0) as f32, size.height.0.max(0) as f32);

        if iw <= 0.0 || ih <= 0.0 {
            continue;
        }

        for k in 0..p.grid_rows {
            let row = p.screen_row as usize + k as usize;
            if !visible.contains(&row) {
                continue;
            }

            let sy0 = p.source_y + p.source_height.saturating_mul(k) / p.grid_rows;
            let sy1 = p.source_y + p.source_height.saturating_mul(k + 1) / p.grid_rows;

            out.push(FrozenImage {
                generation: generation.clone(),
                z: p.z,
                y: pad + row as f32 * cell_h,
                col: p.screen_col,
                width: p.grid_cols,
                source: [
                    p.source_x as f32 / iw,
                    sy0 as f32 / ih,
                    ((p.source_x + p.source_width) as f32 / iw).min(1.0),
                    (sy1 as f32 / ih).min(1.0),
                ],
            });
        }
    }
    out
}
