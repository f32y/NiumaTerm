use crate::terminal::metrics;

pub(super) const BLOCK_SUCCESS_COLOR: u32 = 0xa3be8c;
pub(super) const BLOCK_FAILURE_COLOR: u32 = 0xbf616a;
pub(super) const BLOCK_RUNNING_COLOR: u32 = 0x88c0d0;
pub(super) const BLOCK_INPUT_COLOR: u32 = 0xebcb8b;
pub(super) const BLOCK_SELECTED_TINT: u32 = 0xffffff0d;

/// 1px separator rule inside the gap.
pub(super) const SEPARATOR_COLOR: u32 = 0x3b4252;

/// Width of the block gutter hit band / strip, in px left of the content origin
/// (inside the pane's padding).
pub(super) const BLOCK_GUTTER_WIDTH: f32 = 4.0;
/// Gap between the gutter strip and the text; GAP + WIDTH = PADDING_PX so the
/// strip sits flush against the pane's left edge.
pub(super) const BLOCK_GUTTER_GAP: f32 = metrics::PADDING_PX - BLOCK_GUTTER_WIDTH;
