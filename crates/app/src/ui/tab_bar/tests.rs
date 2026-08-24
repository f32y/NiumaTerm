use gpui::px;

use crate::ui::UI_RADIUS;
use crate::ui::tab_bar::{
    AgentTabIndicator, COMPACT_TAB_WIDTH, FULL_TAB_WIDTH, MIN_AUTO_TAB_WIDTH, NEW_TAB_BUTTON_WIDTH,
    TAB_BAR_PADDING, TAB_GAP, TabDensity, agent_tab_indicator, auto_tab_width, progress_bar_width,
    tab_density,
};

#[test]
fn progress_bar_stops_at_rounded_tab_edges() {
    let tab_width = px(150.0);
    let bar_width = progress_bar_width(tab_width);

    assert_eq!(bar_width, px(134.0));
    assert_eq!(tab_width - UI_RADIUS - bar_width, UI_RADIUS);
}

/// Auto Size holds the configured width while the row has room, shares the
/// row once it does not, and stops at the width that still shows the leading
/// icon. It also survives a strip that has not been measured yet.
#[test]
fn auto_size_shares_the_strip_between_tabs() {
    let configured = 120.0;
    let width = |strip: f32, count: usize| auto_tab_width(strip, count, configured);

    assert_eq!(width(1200.0, 2), configured);

    let crowded = width(800.0, 8);
    assert!(
        crowded < configured,
        "{crowded} should be under {configured}"
    );
    assert!(crowded > MIN_AUTO_TAB_WIDTH);
    assert!(
        (crowded * 8.0 + TAB_GAP * 8.0 + TAB_BAR_PADDING + NEW_TAB_BUTTON_WIDTH - 800.0).abs()
            < 0.001,
        "the tabs and their gaps should consume the strip exactly",
    );

    assert_eq!(width(800.0, 40), MIN_AUTO_TAB_WIDTH);
    assert_eq!(width(0.0, 8), configured);
    assert_eq!(width(1200.0, 0), configured);
    assert_eq!(auto_tab_width(800.0, 40, 30.0), 30.0);
}

#[test]
fn a_shrinking_tab_gives_up_the_title_first() {
    assert_eq!(tab_density(120.0), TabDensity::Full);
    assert_eq!(tab_density(FULL_TAB_WIDTH), TabDensity::Full);
    assert_eq!(tab_density(FULL_TAB_WIDTH - 1.0), TabDensity::Compact);
    assert_eq!(tab_density(COMPACT_TAB_WIDTH), TabDensity::Compact);
    assert_eq!(tab_density(COMPACT_TAB_WIDTH - 1.0), TabDensity::IconOnly);
    assert!(MIN_AUTO_TAB_WIDTH < COMPACT_TAB_WIDTH);
    assert_eq!(tab_density(MIN_AUTO_TAB_WIDTH), TabDensity::IconOnly);
}

#[test]
fn busy_indicator_takes_precedence_over_ready() {
    assert_eq!(
        agent_tab_indicator(true, true),
        Some(AgentTabIndicator::Busy)
    );
    assert_eq!(
        agent_tab_indicator(false, true),
        Some(AgentTabIndicator::Ready)
    );
    assert_eq!(agent_tab_indicator(false, false), None);
}
