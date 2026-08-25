//! Measurements transcribed from the WinUI theme resources that ship with the
//! Windows App SDK (`Microsoft.UI/Themes/generic.xaml`), so this application's
//! chrome measures the same as the navigation surfaces Windows draws itself.
//!
//! Each entry names the resource key it came from, which is what makes a value
//! checkable: a number that looks wrong can be compared against the system
//! resource rather than re-derived by eye. Values the application chooses for
//! itself — its own gutters and column widths — belong in
//! `crate::ui::composition` instead, because changing one of those is a design
//! decision while changing one of these is correcting a transcription.

use gpui::{Pixels, px};

/// `ControlCornerRadius`: buttons, list rows, and input fields. Larger
/// surfaces (dialogs, cards, the pane frame) use `crate::ui::UI_RADIUS`.
pub(crate) const CONTROL_RADIUS: Pixels = px(4.0);

/// `ButtonPadding` (11px) plus the 1px control stroke it sits inside, which is
/// what the eye measures from the button's outer edge.
pub(crate) const BUTTON_PADDING_X: Pixels = px(12.0);

/// `NavigationViewSelectionIndicator{Width,Height,Radius}`: the accent mark on
/// the leading edge of a selected navigation row. Windows keeps this geometry
/// fixed at every row height, so rows of different heights carry the same mark
/// and line up as one column of selection cues.
pub(crate) const SELECTION_BAR_WIDTH: f32 = 3.0;
pub(crate) const SELECTION_BAR_HEIGHT: f32 = 16.0;
pub(crate) const SELECTION_BAR_RADIUS: f32 = 2.0;
