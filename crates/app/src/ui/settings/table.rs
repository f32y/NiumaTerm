//! Shared look of the settings tables: the frame, the column header strip,
//! the row metrics, and the row-operation controls. Two tables use it — the
//! agent profiles and a profile's environment variables — and they are meant
//! to read as the same object, so their measurements live in one place.

use gpui::prelude::FluentBuilder as _;
use gpui::{App, Div, Pixels, SharedString, Styled as _, px};
use gpui_component::{ActiveTheme as _, IconNamed, h_flex};

use crate::ui::UI_RADIUS;

/// Content height of one row. A table whose rows are virtualized needs every
/// row to agree on a height, so this is stated rather than measured.
pub(super) const TABLE_ROW_CONTENT_HEIGHT: Pixels = px(32.0);

/// Height one row occupies: the content plus the vertical padding around it.
pub(super) const TABLE_ROW_HEIGHT: f32 = 40.0;

/// Header height. It seats text rather than controls, so it stays compact
/// instead of following the row height.
pub(super) const TABLE_HEADER_HEIGHT: f32 = 32.0;

/// Operation-column glyph size. `Button` draws its icon at three quarters of
/// the button size, so the control is sized to land the glyph on this value.
pub(super) const TABLE_OPERATION_ICON: f32 = 16.0;
pub(super) const TABLE_OPERATION_BUTTON: Pixels = px(TABLE_OPERATION_ICON / 0.75);

/// Operation-column width for a table whose rows carry a single control.
pub(super) const ENV_OPERATION_COLUMN: Pixels = px(72.0);

/// Delete glyph, backed by the project's `assets/icons/trash.svg`. The icon
/// set gpui-component ships names a backspace arrow `Delete`, which reads as
/// "clear the field" rather than "remove this row".
pub(super) struct TrashIcon;

impl IconNamed for TrashIcon {
    fn path(self) -> SharedString {
        "icons/trash.svg".into()
    }
}

/// The bordered box a table sits in. It clips the header fill and the row
/// rules to the rounded corners, so the table reads as one object.
pub(super) fn table_frame(cx: &App) -> Div {
    gpui::div()
        .w_full()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(UI_RADIUS)
        .overflow_hidden()
}

/// The column-title strip. Callers add one cell per column, using the same
/// widths their rows use.
pub(super) fn table_header(cx: &App) -> Div {
    h_flex()
        .w_full()
        .h(px(TABLE_HEADER_HEIGHT))
        .px_3()
        .gap_2()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted.opacity(0.4))
        .text_xs()
        .text_color(cx.theme().muted_foreground)
}

/// One table row, padded to match the header. `ruled` draws the divider to
/// the next row; the frame supplies the last row's bottom edge, so repeating
/// it there would double the line.
pub(super) fn table_row(ruled: bool, cx: &App) -> Div {
    h_flex()
        .w_full()
        .h(TABLE_ROW_CONTENT_HEIGHT)
        .px_3()
        .py_1()
        .gap_2()
        .items_center()
        .when(ruled, |this| {
            this.border_b_1().border_color(cx.theme().border)
        })
}
