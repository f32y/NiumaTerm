use gpui::{AnyElement, Div, Stateful, Styled as _, px};
use gpui_component::TitleBar;
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::tab::{Tab, TabBar};

use crate::ui::platform_style::PlatformStyle;
use crate::ui::tab_bar::TabDensity;

/// The caption controls are part of the title bar at its trailing edge, so
/// the chrome starts at the window's leading edge and the tab strip keeps the
/// variant's own spacing.
pub(crate) struct Windows;

impl PlatformStyle for Windows {
    const WINDOW_CONTROLS_INSET: Option<f32> = None;

    /// The leading region starts on the window's edge, so the controls in it
    /// stand on the same column as the sidebar below.
    const TITLE_BAR_LEADING_INSET: f32 = 0.0;

    const TITLE_BAR_TRAILING_INSET: f32 = 0.0;

    /// The leading icon, the variant's content padding and the close control
    /// side by side (2 + 12 + 4 + 32 + 4 + 16).
    const COMPACT_TAB_WIDTH: f32 = 70.0;

    const FULL_TAB_WIDTH: f32 = 100.0;

    /// The bar carries a small leading padding of its own, which is the gap
    /// the first control needs away from the window edge.
    fn title_bar(bar: TitleBar) -> TitleBar {
        bar
    }

    fn tab_bar(bar: TabBar) -> TabBar {
        bar
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

    /// The quota row stays on the sidebar's content column.
    fn sidebar_agent_usage(usage: AnyElement) -> AnyElement {
        usage
    }

    /// Symmetric padding keeps the quota text off both edges of the row fill.
    fn agent_usage_row(row: Button) -> Button {
        row.px_1()
    }
}
