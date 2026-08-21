//! Sizing and placement for the modern context menu.
//!
//! Every dimension here is resolved before the menu window exists. `PlatformWindow`
//! can resize a window but has no way to move one, and its resize is queued on the
//! platform executor, so a menu that measured itself after creation would present
//! at least one frame at the wrong place. Because the menu has no submenus and no
//! separators, its size is a plain function of the widest label and the item count.

use gpui::{Bounds, Pixels, Point, Size, point, px, size};

/// Row height of a menu item, matching the Windows 11 flyout metric at 100% scale.
pub(super) const ITEM_HEIGHT: Pixels = px(32.0);
/// Space between the menu edge and the first and last row.
pub(super) const MENU_PADDING: Pixels = px(4.0);
/// Horizontal inset of the item label.
pub(super) const ITEM_PADDING_X: Pixels = px(12.0);
/// The column every row keeps for an icon, and the gap between it and the label.
///
/// Reserved whether or not a given item has an icon, which is what keeps the
/// labels of one menu aligned with each other. Measured off a system context
/// menu at 150% scale: its icons sit 16 in from the menu's inner edge and its
/// labels start at 43, leaving 11 between the two.
pub(super) const ICON_SIZE: Pixels = px(16.0);
pub(super) const ICON_GAP: Pixels = px(11.0);

/// A command row's height, and the width and label size of the buttons in it.
///
/// The row that a system context menu opens with, holding the few actions worth
/// reaching without reading: an icon over a small label, laid left to right at a
/// fixed width rather than stretched, so the buttons stay the same size whatever
/// the menu's labels do to its width. Measured off the same system menu, whose
/// buttons repeat every 65 and whose row runs 58 deep.
pub(super) const COMMAND_ROW_HEIGHT: Pixels = px(58.0);
pub(super) const COMMAND_BUTTON_WIDTH: Pixels = px(65.0);
pub(super) const COMMAND_LABEL_SIZE: Pixels = px(11.0);
/// Space between a command button's icon and its label.
pub(super) const COMMAND_LABEL_GAP: Pixels = px(6.0);

/// Width of the menu's outline. Part of what insets the rows, so `menu_size`
/// counts it; a label measured without it is wider than the room it gets and
/// loses its last glyph to the window edge.
pub(super) const STROKE_WIDTH: Pixels = px(1.0);

/// Height a separator occupies: a one pixel rule with room either side of it.
/// Measured off a system context menu, whose rule sits six device pixels clear of
/// the rows above and below it at 150% scale.
pub(super) const SEPARATOR_HEIGHT: Pixels = px(9.0);
/// Thickness of the rule itself, drawn in the same color as the menu's outline.
pub(super) const SEPARATOR_THICKNESS: Pixels = px(1.0);

/// Radius of an item's hover fill.
pub(super) const ITEM_RADIUS: Pixels = px(4.0);
/// Label size.
pub(super) const FONT_SIZE: Pixels = px(14.0);

/// Radius of the menu's outline, matching the frame DWM rounds the window to.
/// A different value here would show as a hairline that drifts away from the
/// corner it is supposed to trace.
pub(super) const CORNER_RADIUS: Pixels = px(8.0);

/// How much of the menu surface is the theme's own popover color rather than the
/// blurred material under it.
///
/// Two things pull against each other here. The tint is what puts a floor under
/// label contrast, since a menu opens over terminal output of any brightness and
/// the blur alone does not bound it. Past roughly two thirds, though, the tint
/// covers the material outright and the menu reads as a plain opaque panel.
///
/// This sits low enough for the blur to stay visible as a material. Raise it if
/// labels wash out over bright content.
pub(super) const TINT_ALPHA: f32 = 0.35;

/// Alpha of that hairline, which is always black.
///
/// A flyout reads as lifted through two things: the shadow DWM draws around the
/// window, and a darkened hairline along its edge that separates the surface from
/// whatever it covers. Darkening rather than lightening is what the system
/// flyouts do in both themes; a light edge on a dark surface reads as a drawn
/// border instead of a gap.
///
/// The light value is measured off a system context menu: its edge sits 12 levels
/// under the 252 surface behind it, on both the top and the left. The dark value
/// is heavier because the same separation has to survive against a dark surface.
pub(super) fn stroke_alpha(dark: bool) -> f32 {
    if dark { 0.2 } else { 0.048 }
}

/// Narrow menus still read as menus rather than as tooltips.
const MIN_LABEL_WIDTH: Pixels = px(96.0);
/// Past this the label is ellipsized; a menu wider than this stops being scannable.
const MAX_LABEL_WIDTH: Pixels = px(320.0);

/// Distance kept from the work area edge when the menu has to be clamped, so the
/// drop shadow DWM draws for the window is not cut off by the screen edge.
const EDGE_MARGIN: Pixels = px(4.0);

/// What a menu holds, which together with its widest label decides its size.
#[derive(Default, Clone, Copy)]
pub(super) struct Content {
    pub items: usize,
    pub separators: usize,
    pub command_rows: usize,
    /// Buttons in the command row that has the most of them, which is what a
    /// command row needs the menu to be wide enough for.
    pub widest_command_row: usize,
}

/// Outer size of a menu whose widest label shapes to `widest_label`.
pub(super) fn menu_size(widest_label: Pixels, content: Content) -> Size<Pixels> {
    let label = widest_label.max(MIN_LABEL_WIDTH).min(MAX_LABEL_WIDTH);
    // A command row spans the same area the item rows do but keeps none of their
    // icon column, so the two ask for different widths and the menu takes the
    // larger.
    let rows = label + ICON_SIZE + ICON_GAP + ITEM_PADDING_X * 2.0;
    let commands = COMMAND_BUTTON_WIDTH * content.widest_command_row as f32;

    size(
        rows.max(commands) + (MENU_PADDING + STROKE_WIDTH) * 2.0,
        ITEM_HEIGHT * content.items as f32
            + SEPARATOR_HEIGHT * content.separators as f32
            + COMMAND_ROW_HEIGHT * content.command_rows as f32
            + MENU_PADDING * 2.0,
    )
}

/// Top-left corner for a menu of `menu` size opened at `anchor`.
///
/// The menu opens down and to the right of the anchor, and flips to the opposite
/// side when that would leave the work area. Flipping rather than sliding keeps
/// the anchor on a corner of the menu, so the pointer never ends up resting on an
/// item the user did not aim at. Clamping is the last resort, for a menu taller
/// or wider than the space on either side of the anchor.
pub(super) fn place(
    anchor: Point<Pixels>,
    menu: Size<Pixels>,
    work_area: Bounds<Pixels>,
) -> Point<Pixels> {
    let left = work_area.origin.x + EDGE_MARGIN;
    let top = work_area.origin.y + EDGE_MARGIN;
    let right = work_area.origin.x + work_area.size.width - EDGE_MARGIN;
    let bottom = work_area.origin.y + work_area.size.height - EDGE_MARGIN;

    let x = if anchor.x + menu.width > right {
        anchor.x - menu.width
    } else {
        anchor.x
    };
    let y = if anchor.y + menu.height > bottom {
        anchor.y - menu.height
    } else {
        anchor.y
    };

    point(
        clamp(x, left, right - menu.width),
        clamp(y, top, bottom - menu.height),
    )
}

/// `low` wins when the menu is larger than the range, which keeps the top-left
/// corner on screen and lets the overflow run off the far edge.
fn clamp(value: Pixels, low: Pixels, high: Pixels) -> Pixels {
    if high < low {
        low
    } else {
        value.max(low).min(high)
    }
}
