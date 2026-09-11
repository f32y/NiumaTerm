use crate::pane_model::viewport::LocalPoint;
use crate::theme::{BLOCK_GUTTER_GAP, BLOCK_GUTTER_WIDTH};

/// A pointer x hits the block gutter when it falls in the strip painted in the
/// left padding, with a small tolerance into column 0.
pub(crate) fn block_gutter_hit(x: f32, origin_x: f32) -> bool {
    let left = origin_x - BLOCK_GUTTER_GAP - BLOCK_GUTTER_WIDTH - 2.0;
    let right = origin_x + 3.0;

    (left..=right).contains(&x)
}

pub(crate) fn selection_drag_started(
    origin: LocalPoint,
    position: LocalPoint,
    cell_width: f32,
) -> bool {
    let dx = position.x - origin.x;
    let dy = position.y - origin.y;

    dx * dx + dy * dy >= cell_width * cell_width / 16.0
}
