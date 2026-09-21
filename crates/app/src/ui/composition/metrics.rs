/// Flush surfaces keep the tab strip and sidebar on one vertical divider.
pub(crate) const FLOATING_SURFACE_SIDE_INSET: f32 = 0.0;

/// The active tab meets the content border without a second horizontal gap.
pub(crate) const FLOATING_SURFACE_TOP_INSET: f32 = 0.0;

/// Bottom gutter for the workspace sidebar, which floats clear of the window
/// edge; the main pane runs into that edge instead.
pub(crate) const FLOATING_SURFACE_BOTTOM_INSET: f32 = 6.0;

/// Toolbar controls fit within the title bar without stretching its height.
pub(crate) const TOOLBAR_BUTTON_SIZE: f32 = 30.0;

/// Edge of the icon a toolbar button centers in itself. The button's pixel
/// size is a style override, so its icon keeps the component's medium
/// 16px size. The sidebar sets its whole content column on the edge of this
/// icon box, which is what lines the section heading, the tab glyphs and
/// the status rows up under the app menu button.
pub(crate) const TOOLBAR_ICON_SIZE: f32 = 16.0;
