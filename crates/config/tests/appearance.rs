use nmt_config::Config;
use nmt_config::appearance::{AppearanceConfig, InputStyle, SmoothScrollingMode, WindowBackdrop};
use toml::from_str;

#[test]
fn appearance_section_defaults_when_absent() {
    let config: Config = from_str("").unwrap();
    assert_eq!(config.appearance, AppearanceConfig::default());
    assert_eq!(config.appearance.input_style, InputStyle::Waterfall);
    assert!(config.appearance.scroll_to_bottom_when_typing);
    assert!(!config.appearance.agent_pane_use_terminal_background);
    assert!(config.appearance.transparent_main_view);
    assert_eq!(config.appearance.smooth_scrolling, SmoothScrollingMode::All);
    assert!(config.profiles.list.is_empty());
}

#[test]
fn scroll_to_bottom_when_typing_defaults_on_and_accepts_false() {
    let missing: Config = from_str("[appearance]\n").unwrap();
    assert!(missing.appearance.scroll_to_bottom_when_typing);

    let disabled: Config =
        from_str("[appearance]\nscroll-to-bottom-when-typing = false\n").unwrap();
    assert!(!disabled.appearance.scroll_to_bottom_when_typing);
}

#[test]
fn smooth_scrolling_defaults_and_accepts_modes_and_legacy_values() {
    let missing: Config = from_str("[appearance]\ncommand-blocks = false\n").unwrap();
    assert_eq!(
        missing.appearance.smooth_scrolling,
        SmoothScrollingMode::All
    );

    for (value, expected) in [
        ("all", SmoothScrollingMode::All),
        ("only-terminal", SmoothScrollingMode::OnlyTerminal),
        ("only-agent", SmoothScrollingMode::OnlyAgent),
        ("off", SmoothScrollingMode::Off),
    ] {
        let config: Config =
            from_str(&format!("[appearance]\nsmooth-scrolling = \"{value}\"\n")).unwrap();
        assert_eq!(config.appearance.smooth_scrolling, expected);
    }

    let legacy_on: Config = from_str("[appearance]\nsmooth-scrolling = true\n").unwrap();
    assert_eq!(
        legacy_on.appearance.smooth_scrolling,
        SmoothScrollingMode::All
    );
    let legacy_off: Config = from_str("[appearance]\nsmooth-scrolling = false\n").unwrap();
    assert_eq!(
        legacy_off.appearance.smooth_scrolling,
        SmoothScrollingMode::Off
    );
}

#[test]
fn window_backdrop_defaults_and_accepts_modes_and_legacy_values() {
    let missing: Config = from_str("").unwrap();
    assert_eq!(missing.appearance.window_backdrop, WindowBackdrop::Acrylic);

    for (value, expected) in [
        ("mica", WindowBackdrop::Mica),
        ("acrylic", WindowBackdrop::Acrylic),
        ("off", WindowBackdrop::Off),
    ] {
        let config: Config = from_str(&format!(
            "[appearance]\nenable-window-transparency = \"{value}\"\n"
        ))
        .unwrap();
        assert_eq!(config.appearance.window_backdrop, expected);
    }

    // Legacy boolean values: on preserved the acrylic behavior, off was opaque.
    let legacy_on: Config = from_str("[appearance]\nenable-window-transparency = true\n").unwrap();
    assert_eq!(
        legacy_on.appearance.window_backdrop,
        WindowBackdrop::Acrylic
    );
    let legacy_off: Config =
        from_str("[appearance]\nenable-window-transparency = false\n").unwrap();
    assert_eq!(legacy_off.appearance.window_backdrop, WindowBackdrop::Off);
}

#[test]
fn smooth_scrolling_modes_select_the_expected_views() {
    assert!(SmoothScrollingMode::All.terminal_enabled());
    assert!(SmoothScrollingMode::All.agent_enabled());
    assert!(SmoothScrollingMode::OnlyTerminal.terminal_enabled());
    assert!(!SmoothScrollingMode::OnlyTerminal.agent_enabled());
    assert!(!SmoothScrollingMode::OnlyAgent.terminal_enabled());
    assert!(SmoothScrollingMode::OnlyAgent.agent_enabled());
    assert!(!SmoothScrollingMode::Off.terminal_enabled());
    assert!(!SmoothScrollingMode::Off.agent_enabled());
}
