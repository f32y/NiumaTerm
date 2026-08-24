use gpui::{App, Hsla, StyleRefinement, Styled as _};
use gpui_component::ActiveTheme as _;

use crate::ui::UI_RADIUS;

#[derive(Clone, Copy)]
pub(crate) struct SidebarSelection {
    pub(crate) active_background: Hsla,
    pub(crate) active_foreground: Hsla,
    pub(crate) idle_foreground: Hsla,
    pub(crate) hover_background: Hsla,
}

/// Workspace buttons and vertical tab rows use the same selection language
/// even though their component types require different style application.
pub(crate) fn sidebar_selection(cx: &App) -> SidebarSelection {
    SidebarSelection {
        active_background: cx.theme().sidebar_accent,
        active_foreground: cx.theme().sidebar_accent_foreground,
        idle_foreground: cx.theme().sidebar_foreground.opacity(0.75),
        hover_background: cx.theme().sidebar_accent.opacity(0.4),
    }
}

/// Outer frame shared by pane and sidebar surfaces. Callers supply size and
/// semantic background because those values differ by host.
pub(crate) fn surface_frame(cx: &App) -> StyleRefinement {
    StyleRefinement::default()
        .border_1()
        .border_color(cx.theme().sidebar_border)
        .rounded(UI_RADIUS)
        .overflow_hidden()
}

/// A full bordered region whose children must stay clipped to rounded edges.
pub(crate) fn framed_region(cx: &App) -> StyleRefinement {
    StyleRefinement::default()
        .w_full()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(UI_RADIUS)
        .overflow_hidden()
}

/// Shared title strip for the interchangeable right-side panel contents.
pub(crate) fn panel_header(cx: &App) -> StyleRefinement {
    StyleRefinement::default()
        .px_2()
        .py_1()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().sidebar_border)
}

/// Sidebar-content surface shared by the right panel and Settings navigation.
/// Callers retain ownership of size and any edge they intentionally suppress.
pub(crate) fn sidebar_surface(cx: &App) -> StyleRefinement {
    surface_frame(cx).bg(cx.theme().sidebar)
}

/// Common table heading treatment. Callers retain ownership of height and text
/// size because those measurements differ between settings and usage tables.
pub(crate) fn table_header(cx: &App) -> StyleRefinement {
    StyleRefinement::default()
        .w_full()
        .px_3()
        .gap_2()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted.opacity(0.4))
        .text_color(cx.theme().muted_foreground)
}
