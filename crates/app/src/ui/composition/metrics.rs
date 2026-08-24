/// The workspace and main pane use the same horizontal gutter so their outer
/// edges read as peer surfaces when both are visible.
pub(crate) const FLOATING_SURFACE_SIDE_INSET: f32 = 6.0;

/// The top gutter keeps the main pane aligned with the independent tab strip.
pub(crate) const FLOATING_SURFACE_TOP_INSET: f32 = 1.0;

/// The shared bottom gutter keeps the workspace and main pane frames aligned.
pub(crate) const FLOATING_SURFACE_BOTTOM_INSET: f32 = 6.0;
