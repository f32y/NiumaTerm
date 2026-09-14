//! Shared geometry for application chrome and conversation surfaces.

use gpui::{Pixels, px};

pub const CONTROL_RADIUS: Pixels = px(6.0);
pub const SURFACE_RADIUS: Pixels = px(8.0);
pub const CARD_RADIUS: Pixels = px(12.0);
pub const TAB_HEIGHT: Pixels = px(32.0);

/// The settings tab ends on the navigation pane's content divider.
pub const SETTINGS_NAV_WIDTH: Pixels = px(240.0);

/// Adjacent file and review panes share a footer baseline at every window size.
pub const REVIEW_FOOTER_HEIGHT: Pixels = px(32.0);

/// Repeated layout spacing follows a four-pixel scale.
pub const SPACE_2: Pixels = px(8.0);

pub const SPACE_3: Pixels = px(12.0);

/// Auxiliary views yield space before the main reading area becomes cramped.
pub const PRIMARY_CONTENT_MIN_WIDTH: Pixels = px(320.0);

pub const AUXILIARY_MIN_WIDTH: Pixels = px(240.0);
pub const AUXILIARY_MAX_WIDTH: Pixels = px(480.0);

/// A starting proportion for an auxiliary view, before content bounds or user resizing.
pub const AUXILIARY_WIDTH_SHARE: f32 = 0.381_966;

/// File navigation leaves most of the review surface available for code.
pub const REVIEW_FILES_WIDTH: Pixels = px(224.0);

pub const REVIEW_FILES_MIN_WIDTH: Pixels = px(160.0);
pub const REVIEW_FILES_MAX_WIDTH: Pixels = px(400.0);

/// Compact previews compare multiple palettes without imposing an aspect ratio.
pub const THEME_CARD_MIN_WIDTH: Pixels = px(200.0);

pub const THEME_CARD_HEIGHT: Pixels = px(120.0);
pub const THEME_PREVIEW_HEIGHT: Pixels = px(72.0);
pub const THEME_GRID_MAX_COLUMNS: u16 = 4;
