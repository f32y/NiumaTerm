use gpui::px;
use nmt_config::profile::Profile;

use crate::ui::UI_RADIUS;
use crate::ui::tab_bar::menu::profile_root_choices;
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
    const { assert!(MIN_AUTO_TAB_WIDTH < COMPACT_TAB_WIDTH) };
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

/// A terminal Profile named `name` running `shell`.
fn profile(name: &str, shell: &str) -> Profile {
    Profile {
        name: name.to_string(),
        shell: shell.to_string(),
        args: String::new(),
    }
}

/// Reachable workspace directories in the given order.
fn roots(paths: &[&str]) -> Vec<(String, bool)> {
    paths.iter().map(|path| (path.to_string(), true)).collect()
}

#[test]
fn combinations_are_profile_major_and_cover_every_directory() {
    let profiles = [profile("P1", "pwsh.exe"), profile("P2", "cmd.exe")];
    let choices = profile_root_choices(&profiles, &roots(&["C:/A", "C:/B", "C:/C"]));

    assert_eq!(choices.len(), 6);
    let pairs: Vec<_> = choices
        .iter()
        .map(|choice| (choice.launch.0.as_deref().unwrap(), choice.cwd.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("pwsh.exe", "C:/A"),
            ("pwsh.exe", "C:/B"),
            ("pwsh.exe", "C:/C"),
            ("cmd.exe", "C:/A"),
            ("cmd.exe", "C:/B"),
            ("cmd.exe", "C:/C"),
        ]
    );
}

#[test]
fn a_profile_without_a_command_contributes_no_combination() {
    let profiles = [
        profile("P1", "pwsh.exe"),
        profile("Empty", "   "),
        profile("P2", "cmd.exe"),
    ];
    let choices = profile_root_choices(&profiles, &roots(&["C:/A", "C:/B"]));

    assert_eq!(choices.len(), 4);
    assert!(choices.iter().all(|choice| choice.label.contains('P')));
}

#[test]
fn each_combination_launches_in_exactly_the_selected_directory() {
    let profiles = [profile("P1", "pwsh.exe")];
    let choices = profile_root_choices(&profiles, &roots(&[r"C:\Work\api", r"D:\Docs\api"]));

    // Two directories sharing a final component stay distinguishable because
    // every label carries the full path.
    assert_eq!(choices[0].cwd, r"C:\Work\api");
    assert_eq!(choices[1].cwd, r"D:\Docs\api");
    assert!(choices[0].label.contains(r"C:\Work\api"));
    assert!(choices[1].label.contains(r"D:\Docs\api"));
    assert_ne!(choices[0].label, choices[1].label);
    assert!(choices.iter().all(|choice| choice.label.contains("P1")));
}

#[test]
fn combinations_of_an_unavailable_directory_stay_listed_and_disabled() {
    let profiles = [profile("P1", "pwsh.exe"), profile("P2", "cmd.exe")];
    let roots = vec![
        ("C:/A".to_string(), true),
        ("Z:/detached".to_string(), false),
    ];
    let choices = profile_root_choices(&profiles, &roots);

    assert_eq!(choices.len(), 4);
    let enabled: Vec<_> = choices.iter().map(|choice| choice.enabled).collect();
    assert_eq!(enabled, [true, false, true, false]);
}
