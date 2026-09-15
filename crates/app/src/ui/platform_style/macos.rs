use app::design::{SPACE_2, SPACE_3};
use gpui::prelude::*;
use gpui::{AnyElement, Div, Edges, Stateful, div, px};
use gpui_component::TitleBar;
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::tab::{Tab, TabBar};

use crate::ui::platform_style::PlatformStyle;
use crate::ui::shell::TITLE_BAR_HEIGHT;
use crate::ui::tab_bar::TabDensity;
use crate::ui::workspace_sidebar::SIDEBAR_ROW_GUTTER;

/// Height of a close/minimize/zoom button, measured from the frame AppKit
/// gives the standard window buttons. Their vertical inset is applied
/// symmetrically from the top of the window, so half the leftover space
/// centers them in the taller title bar.
const TRAFFIC_LIGHT_HEIGHT: f32 = 14.0;

/// AppKit keeps drawing the window buttons over the transparent title bar,
/// so the chrome makes room for them at the leading edge.
pub(crate) struct MacOs;

impl PlatformStyle for MacOs {
    /// Equal top and leading insets center the close button within the
    /// window's rounded corner instead of keeping the narrower inset AppKit
    /// applies for a standard-height title bar.
    const WINDOW_CONTROLS_INSET: Option<f32> =
        Some((TITLE_BAR_HEIGHT - TRAFFIC_LIGHT_HEIGHT) / 2.0);

    /// The three window buttons and their spacing occupy the leading edge.
    const TITLE_BAR_LEADING_INSET: f32 = 80.0;

    /// Keeps toggled and hovered controls inside the window's curved edge.
    const TITLE_BAR_TRAILING_INSET: f32 = 12.0;

    /// The icon and the close control each keep a real inset, so a tab needs
    /// that much more room before it gives both up for the glyph slot.
    const COMPACT_TAB_WIDTH: f32 = 82.0;

    const FULL_TAB_WIDTH: f32 = 112.0;

    /// State the reserved leading edge on the bar itself, so the room kept
    /// clear for the window buttons and the width the leading region is
    /// measured against cannot drift apart.
    fn title_bar(bar: TitleBar) -> TitleBar {
        bar.pl(px(Self::TITLE_BAR_LEADING_INSET))
    }

    /// The bar already starts past the window buttons, so the variant's own
    /// leading padding would open a second gap before the first tab.
    fn tab_bar(bar: TabBar) -> TabBar {
        bar.pl_0()
    }

    /// The title sits close to the icon while the trailing padding keeps the
    /// close control clear of the tab's edge. An icon-only tab has no title
    /// area left to pad.
    fn tab(tab: Tab, density: TabDensity) -> Tab {
        match density {
            TabDensity::IconOnly => tab,
            TabDensity::Full | TabDensity::Compact => tab.content_paddings(Edges {
                left: px(4.0),
                right: SPACE_3,
                ..Default::default()
            }),
        }
    }

    /// The inset takes layout room, so the title laid out after the icon
    /// keeps its distance from it.
    fn tab_prefix(prefix: Div) -> Div {
        prefix.pl(SPACE_3)
    }

    /// Mirrors the leading inset at the trailing edge. An icon-only tab hands
    /// its one slot to the close control and has no trailing group to inset.
    fn tab_suffix(suffix: Div, density: TabDensity) -> Div {
        match density {
            TabDensity::IconOnly => suffix,
            TabDensity::Full | TabDensity::Compact => suffix.pr(SPACE_2),
        }
    }

    /// Titles start right after the icon, so every title in the strip begins
    /// at the same offset. A lone glyph stays centered in its slot.
    fn tab_title(title: Stateful<Div>, density: TabDensity) -> Stateful<Div> {
        match density {
            TabDensity::IconOnly => title.justify_center(),
            TabDensity::Full | TabDensity::Compact => title.justify_start(),
        }
    }

    /// Matches the leading title, so renaming edits the text where it already
    /// stands.
    fn tab_rename_input(input: Input) -> Input {
        input.text_left()
    }

    /// The heading rests on the bottom edge the attached tab strip occupies,
    /// so switching between tab layouts keeps the bar's content on one line.
    fn session_heading_slot(slot: Div) -> Div {
        slot
    }

    /// The row reaches back into the gutter the workspace rows' fills use, so
    /// its hover fill starts on the same edge as theirs.
    fn sidebar_agent_usage(usage: AnyElement) -> AnyElement {
        div()
            .ml(px(-SIDEBAR_ROW_GUTTER))
            .child(usage)
            .into_any_element()
    }

    /// The sidebar already offsets the row into its gutter, so the row adds no
    /// leading padding or border of its own and its text keeps the column.
    fn agent_usage_row(row: Button) -> Button {
        row.pl_0().pr_1().border_0()
    }
}
