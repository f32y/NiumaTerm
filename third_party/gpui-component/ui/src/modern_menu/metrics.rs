//! Sizing and placement for the modern context menu.
//!
//! Every dimension here is resolved before the menu window exists. `PlatformWindow`
//! can resize a window but has no way to move one, and its resize is queued on the
//! platform executor, so a menu that measured itself after creation would present
//! at least one frame at the wrong place. Its size is a plain function of the
//! widest label and the counts of each row kind.

use gpui::{Bounds, Pixels, Point, Size, point, px, size};

use crate::modern_menu::ModernMenuInput;

/// Vertical inset of the item list inside the presenter.
pub(super) const PRESENTER_PADDING_Y: Pixels = px(2.0);
/// Stroke drawn inside the DWM-clipped popup surface.
pub(super) const BORDER_WIDTH: Pixels = px(1.0);
/// Space between an item background and the presenter edge.
pub(super) const ITEM_MARGIN_X: Pixels = px(4.0);
/// Space between adjacent item backgrounds.
pub(super) const ITEM_MARGIN_Y: Pixels = px(2.0);
/// Horizontal inset of an item's content.
pub(super) const ITEM_PADDING_X: Pixels = px(11.0);
/// The column every row keeps for an icon, and the gap between it and the label.
///
/// Reserved whether or not a given item has an icon, which is what keeps the
/// labels of one menu aligned with each other. WinUI reserves 28 pixels for the
/// placeholder, leaving 12 after the 16-pixel icon.
pub(super) const ICON_SIZE: Pixels = px(16.0);
pub(super) const ICON_GAP: Pixels = px(12.0);

/// Total row bands include the two-pixel margin above and below the fill.
const COMPACT_ITEM_HEIGHT: Pixels = px(32.0);
const TOUCH_ITEM_HEIGHT: Pixels = px(40.0);

pub(super) fn item_height(input: ModernMenuInput) -> Pixels {
    match input {
        ModernMenuInput::Mouse | ModernMenuInput::Keyboard => COMPACT_ITEM_HEIGHT,
        ModernMenuInput::Touch => TOUCH_ITEM_HEIGHT,
    }
}

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

/// Height a separator occupies: a one-pixel rule with one pixel above and below.
pub(super) const SEPARATOR_HEIGHT: Pixels = px(3.0);
/// Thickness of the rule itself.
pub(super) const SEPARATOR_THICKNESS: Pixels = px(1.0);

/// Radius of an item's hover fill.
pub(super) const ITEM_RADIUS: Pixels = px(4.0);
/// Label size.
pub(super) const FONT_SIZE: Pixels = px(14.0);

/// Radius of the menu surface, matching the frame DWM rounds the window to.
/// A larger value would show as a tinted corner poking past the rounded frame,
/// a smaller one as a gap between the surface and that frame.
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

/// Alpha values transcribed from the WinUI menu resources.
pub(super) fn surface_stroke_alpha(dark: bool) -> f32 {
    if dark { 0.2 } else { 15.0 / 255.0 }
}

pub(super) fn separator_alpha(dark: bool) -> f32 {
    if dark { 21.0 / 255.0 } else { 15.0 / 255.0 }
}

pub(super) fn hover_alpha(dark: bool) -> f32 {
    if dark { 15.0 / 255.0 } else { 9.0 / 255.0 }
}

pub(super) fn pressed_alpha(dark: bool) -> f32 {
    if dark { 10.0 / 255.0 } else { 6.0 / 255.0 }
}

/// WinUI applies these floors to the complete presenter, not to its label.
pub(super) const MIN_MENU_WIDTH: Pixels = px(96.0);
pub(super) const TOUCH_MIN_MENU_WIDTH: Pixels = px(240.0);
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
pub(super) fn menu_size(
    widest_label: Pixels,
    content: Content,
    input: ModernMenuInput,
) -> Size<Pixels> {
    let label = widest_label.min(MAX_LABEL_WIDTH);
    // A command row spans the same area the item rows do but keeps none of their
    // icon column, so the two ask for different widths and the menu takes the
    // larger.
    let rows = label
        + ICON_SIZE
        + ICON_GAP
        + ITEM_PADDING_X * 2.0
        + ITEM_MARGIN_X * 2.0
        + BORDER_WIDTH * 2.0;
    let commands = COMMAND_BUTTON_WIDTH * content.widest_command_row as f32
        + ITEM_MARGIN_X * 2.0
        + BORDER_WIDTH * 2.0;
    let min_width = match input {
        ModernMenuInput::Mouse | ModernMenuInput::Keyboard => MIN_MENU_WIDTH,
        ModernMenuInput::Touch => TOUCH_MIN_MENU_WIDTH,
    };

    size(
        rows.max(commands).max(min_width),
        item_height(input) * content.items as f32
            + SEPARATOR_HEIGHT * content.separators as f32
            + COMMAND_ROW_HEIGHT * content.command_rows as f32
            + (PRESENTER_PADDING_Y + BORDER_WIDTH) * 2.0,
    )
}

/// Which side of the anchor a menu takes when there is room for it there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Side {
    /// The anchor is the menu's top edge, which is where a pointer press wants
    /// it: the menu unfolds away from the pointer.
    #[default]
    Below,
    /// The anchor is the menu's bottom edge, for a menu about something the
    /// pointer is not on, such as a text selection the menu must not cover.
    Above,
}

/// Top-left corner for a menu of `menu` size opened at `anchor`.
///
/// The menu opens to the right of the anchor and on `side` of it, and flips to
/// the opposite side when that would leave the work area. Flipping rather than
/// sliding keeps the anchor on a corner of the menu, so the pointer never ends up
/// resting on an item the user did not aim at. Clamping is the last resort, for a
/// menu taller or wider than the space on either side of the anchor.
pub(super) fn place(
    anchor: Point<Pixels>,
    menu: Size<Pixels>,
    work_area: Bounds<Pixels>,
    side: Side,
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
    let y = match side {
        Side::Below if anchor.y + menu.height > bottom => anchor.y - menu.height,
        Side::Below => anchor.y,
        Side::Above if anchor.y - menu.height < top => anchor.y,
        Side::Above => anchor.y - menu.height,
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
