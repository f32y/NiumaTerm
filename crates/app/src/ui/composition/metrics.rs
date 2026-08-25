/// The workspace sidebar and the main pane use the same horizontal gutter on
/// the sides that face other chrome, so their edges read as peer surfaces when
/// both are visible.
pub(crate) const FLOATING_SURFACE_SIDE_INSET: f32 = 3.0;

/// The top gutter keeps the main pane aligned with the independent tab strip.
pub(crate) const FLOATING_SURFACE_TOP_INSET: f32 = 1.0;

/// Bottom gutter for the workspace sidebar, which floats clear of the window
/// edge; the main pane runs into that edge instead.
pub(crate) const FLOATING_SURFACE_BOTTOM_INSET: f32 = 6.0;
