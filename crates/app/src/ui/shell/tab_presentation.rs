use gpui::prelude::*;
use gpui::{Div, ElementId, Stateful, div};
use gpui_component::{Icon, IconName, Sizable as _};
use rust_i18n::t;

/// The sleeping-tab glyph is shared by both tab-bar layouts. Its own label
/// remains available when narrow layout removes the tab title.
pub(in crate::ui) fn pending_tab_icon(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .aria_label(t!("tabbar-tooltip-pending"))
        .flex()
        .items_center()
        .child(Icon::new(IconName::Moon).xsmall())
}
