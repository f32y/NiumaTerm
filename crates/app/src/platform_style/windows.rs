use gpui::{AnyElement, Div, Stateful, Styled as _, px};
use gpui_component::TitleBar;
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::tab::Tab;

use crate::design::SIDEBAR_ROW_GUTTER;
use crate::platform_style::{PlatformStyle, TabDensity};

/// The caption controls are part of the title bar at its trailing edge, so
/// the chrome needs only a small gap at the window's leading edge and the tab
/// strip keeps the variant's own spacing.
pub struct Windows;

impl PlatformStyle for Windows {
    const WINDOW_CONTROLS_INSET: Option<f32> = None;

    /// A small gap keeps the first control off the window edge. The leading
    /// region is measured from past it, so the region still ends on the
    /// sidebar's edge and the first tab starts where the content does.
    const TITLE_BAR_LEADING_INSET: f32 = 8.0;

    const TITLE_BAR_TRAILING_INSET: f32 = 0.0;

    /// The leading icon, the variant's content padding and the close control
    /// side by side (2 + 12 + 4 + 32 + 4 + 16).
    const COMPACT_TAB_WIDTH: f32 = 70.0;

    const FULL_TAB_WIDTH: f32 = 100.0;

    /// State the leading gap on the bar itself rather than relying on the
    /// component's default padding, so the gap and the width the leading
    /// region is measured against cannot drift apart.
    fn title_bar(bar: TitleBar) -> TitleBar {
        bar.pl(px(Self::TITLE_BAR_LEADING_INSET))
    }

    /// The variant's own symmetric content padding frames the centered title.
    fn tab(tab: Tab, _density: TabDensity) -> Tab {
        tab
    }

    /// A relative offset moves the icon off the tab's edge without taking
    /// layout room, so the centered title keeps its position.
    fn tab_prefix(prefix: Div) -> Div {
        prefix.relative().left(px(4.0))
    }

    fn tab_suffix(suffix: Div, _density: TabDensity) -> Div {
        suffix
    }

    /// Titles are centered in their tab, and a lone glyph in its slot.
    fn tab_title(title: Stateful<Div>, _density: TabDensity) -> Stateful<Div> {
        title.justify_center()
    }

    /// Matches the centered title, so renaming edits the text where it already
    /// stands.
    fn tab_rename_input(input: Input) -> Input {
        input.text_center()
    }

    /// The heading centers in the bar, on the same line as the caption button
    /// glyphs that span the bar's full height.
    fn session_heading_slot(slot: Div) -> Div {
        slot.items_center()
    }

    /// The status area already reaches into the gutter, so the row keeps its
    /// place in it.
    fn sidebar_agent_usage(usage: AnyElement) -> AnyElement {
        usage
    }

    /// The status area offsets the row into its gutter, so the row pads its
    /// icon back onto the content column and keeps the text off the trailing
    /// edge.
    fn agent_usage_row(row: Button) -> Button {
        row.pl(px(SIDEBAR_ROW_GUTTER)).pr_1()
    }

    /// The content column starts under the app menu button's icon, so the
    /// heading stays on it.
    fn sidebar_heading(heading: Div) -> Div {
        heading
    }

    /// Both status rows reach back into the gutter the workspace row fills
    /// use, so their hover fills start on the same edge as those fills; each
    /// row pads its icon back onto the content column.
    fn sidebar_status(status: Stateful<Div>) -> Stateful<Div> {
        status.ml(px(-SIDEBAR_ROW_GUTTER))
    }

    /// Matches the quota row under it, so the two icons share the content
    /// column.
    fn token_usage_row(row: Button) -> Button {
        Self::agent_usage_row(row)
    }
}
