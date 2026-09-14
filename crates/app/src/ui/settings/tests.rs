use std::collections::BTreeSet;
use std::time::Duration;
use std::{fs, io};

use app::agent_tab::AgentKind;
use app::terminal_tab::settings::TerminalSettings;
use gpui::{
    Context, Entity, IntoElement, ListAlignment, ListOffset, ListState, ScrollDelta,
    ScrollWheelEvent, TestAppContext, list, point, size,
};
use nmt_config::Config;
use nmt_config::appearance::SmoothScrollingMode;
use nmt_config::builtin_themes::{THEMES as BUILTIN_THEMES, get as builtin_theme_source};
use nmt_config::profile::ProfilesConfig;
use nmt_config::theme::Theme as ConfigTheme;

use crate::ui::settings::state::default_shell_for_tests;
use crate::ui::settings::theme::ui_theme_config;
use crate::ui::settings::*;

#[gpui::test]
fn powershell_compatibility_changes_reach_the_live_terminal_snapshot(cx: &mut TestAppContext) {
    cx.set_global(AppSettings::default());
    cx.update(install_terminal_settings);

    assert!(cx.read(|cx| {
        cx.global::<TerminalSettings>()
            .improve_powershell_compatibility
    }));

    cx.update_global::<AppSettings, _>(|settings, _| {
        settings.edit_terminal(|section| section.improve_powershell_compatibility = false);
    });

    assert!(!cx.read(|cx| {
        cx.global::<TerminalSettings>()
            .improve_powershell_compatibility
    }));
}

#[test]
fn cursor_shape_dropdown_values_match_config_shapes() {
    let parsed: CursorShape = "block".into();

    assert_eq!(parsed, CursorShape::Block);

    let parsed: CursorShape = "line".into();

    assert_eq!(parsed, CursorShape::Beam);

    let parsed: CursorShape = "underline".into();

    assert_eq!(parsed, CursorShape::Underline);
}

#[test]
fn tab_width_clamps_to_allowed_range() {
    assert_eq!(clamp_tab_width(MIN_TAB_WIDTH), MIN_TAB_WIDTH);
    assert_eq!(clamp_tab_width(DEFAULT_TAB_WIDTH), DEFAULT_TAB_WIDTH);
    assert_eq!(clamp_tab_width(200.0), 200.0);
    assert_eq!(clamp_tab_width(MAX_TAB_WIDTH), MAX_TAB_WIDTH);
    assert_eq!(clamp_tab_width(10.0), MIN_TAB_WIDTH);
    assert_eq!(clamp_tab_width(9999.0), MAX_TAB_WIDTH);
    assert_eq!(clamp_tab_width(f64::NAN), DEFAULT_TAB_WIDTH);
}

#[test]
fn ui_font_falls_back_when_blank() {
    assert_eq!(ui_font_or_default("Cascadia Code"), "Cascadia Code");
    assert_eq!(ui_font_or_default(""), DEFAULT_UI_FONT);
    assert_eq!(ui_font_or_default("   "), DEFAULT_UI_FONT);
}

#[test]
fn terminal_font_falls_back_when_blank() {
    assert_eq!(terminal_font_or_default("Cascadia Code"), "Cascadia Code");
    assert_eq!(terminal_font_or_default(""), DEFAULT_FONT_FAMILY);
    assert_eq!(terminal_font_or_default("   "), DEFAULT_FONT_FAMILY);
}

#[test]
fn terminal_font_metrics_clamp_to_allowed_range() {
    assert_eq!(clamp_terminal_font_size(16.0), 16.0);
    assert_eq!(clamp_terminal_font_size(1.0), 6.0);
    assert_eq!(clamp_terminal_font_size(100.0), 72.0);
    assert_eq!(clamp_terminal_font_size(f64::NAN), DEFAULT_FONT_SIZE);

    assert_eq!(clamp_terminal_line_height(1.2), 1.2);
    assert_eq!(clamp_terminal_line_height(0.1), 0.8);
    assert_eq!(clamp_terminal_line_height(5.0), 3.0);
    assert_eq!(clamp_terminal_line_height(f64::NAN), DEFAULT_LINE_HEIGHT);

    assert_eq!(clamp_agent_transcript_font_size(12.5), 12.5);
    assert_eq!(clamp_agent_transcript_font_size(1.0), 6.0);
    assert_eq!(clamp_agent_transcript_font_size(100.0), 72.0);
    assert_eq!(
        clamp_agent_transcript_font_size(f64::NAN),
        DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE
    );
}

#[test]
fn agent_transcript_font_has_first_party_defaults() {
    let settings = AppSettings::default();

    assert_eq!(
        settings.config().appearance.agent_transcript_font_family,
        DEFAULT_FONT_FAMILY
    );
    assert_eq!(
        settings.config().appearance.agent_transcript_font_size,
        DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE
    );
}

#[test]
fn window_transparency_controls_opacity_and_blur() {
    assert_eq!(tab_background_opacity(1.0), 1.0);
    assert!((tab_background_opacity(0.65) - 0.825).abs() < f32::EPSILON);
    assert_eq!(clamp_background_opacity(0.1), 0.2);
    assert_eq!(clamp_background_opacity(0.65), 0.65);
    assert_eq!(clamp_background_opacity(2.0), 1.0);
    assert_eq!(clamp_background_opacity(f64::NAN), 1.0);

    // Off keeps the window fully opaque regardless of the slider value.
    assert_eq!(effective_background_opacity(WindowBackdrop::Off, 0.65), 1.0);

    // The Mica materials hand the background to DWM, so a configured opacity is
    // ignored.
    assert_eq!(
        effective_background_opacity(WindowBackdrop::MicaAlt, 0.65),
        0.0
    );
    assert_eq!(
        effective_background_opacity(WindowBackdrop::Mica, 0.65),
        0.0
    );
    assert_eq!(
        effective_background_opacity(WindowBackdrop::Acrylic, 0.65),
        0.65
    );
    assert_eq!(effective_main_view_background_opacity(false, 0.65), 1.0);
    assert_eq!(effective_main_view_background_opacity(true, 0.65), 0.65);
    assert_eq!(clamp_background_image_opacity(-1.0), 0.0);
    assert_eq!(clamp_background_image_opacity(2.0), 1.0);
    assert_eq!(
        clamp_background_image_opacity(f64::NAN),
        DEFAULT_BACKGROUND_IMAGE_OPACITY
    );
    assert_eq!(effective_surface_background_opacity(1.0, None), 1.0);
    assert!((effective_surface_background_opacity(1.0, Some(0.3)) - 0.7).abs() < 1e-12);
    assert_eq!(effective_background_image_layer_opacity(1.0, 0.0), 0.0);
    assert!((effective_background_image_layer_opacity(1.0, 0.3) - 1.0).abs() < 1e-12);

    let surface = effective_surface_background_opacity(0.65, Some(0.3));
    let image = effective_background_image_layer_opacity(0.65, 0.3);

    assert!((surface + (1.0 - surface) * image - 0.65).abs() < 1e-12);
    assert_eq!(
        window_background_appearance_for(WindowBackdrop::Acrylic),
        WindowBackgroundAppearance::Blurred
    );
    assert_eq!(
        window_background_appearance_for(WindowBackdrop::MicaAlt),
        WindowBackgroundAppearance::MicaAltBackdrop
    );
    assert_eq!(
        window_background_appearance_for(WindowBackdrop::Mica),
        WindowBackgroundAppearance::MicaBackdrop
    );
    assert_eq!(
        window_background_appearance_for(WindowBackdrop::Off),
        WindowBackgroundAppearance::Opaque
    );
}

#[test]
fn window_backdrop_value_roundtrip() {
    for backdrop in [
        WindowBackdrop::MicaAlt,
        WindowBackdrop::Mica,
        WindowBackdrop::Acrylic,
        WindowBackdrop::Off,
    ] {
        let value: &str = backdrop.into();
        let parsed: WindowBackdrop = value.into();

        assert_eq!(parsed, backdrop);
    }

    // Unknown values fall back to the opaque mode, which always renders.
    let parsed: WindowBackdrop = "bogus".into();

    assert_eq!(parsed, WindowBackdrop::Off);
}

#[test]
fn git_interval_clamps_to_allowed_set() {
    for v in [10, 15, 30, 60] {
        assert_eq!(clamp_git_interval(v), v);
    }

    for v in [0, 7, 45, 1000] {
        assert_eq!(clamp_git_interval(v), 30);
    }
}

#[test]
fn input_style_value_roundtrip() {
    for style in [InputStyle::Waterfall, InputStyle::FixedBottom] {
        let value: &str = style.into();
        let parsed: InputStyle = value.into();

        assert_eq!(parsed, style);
    }

    // Unknown values fall back to the default style.
    let parsed: InputStyle = "bogus".into();

    assert_eq!(parsed, InputStyle::Waterfall);
}

#[test]
fn load_falls_back_to_default_profile() {
    // Test env has no config file: defaults apply, the empty profiles
    // list maps to the single built-in profile, and the unset default
    // profile resolves to that profile's name.
    let settings = AppSettings::load();

    assert_eq!(
        settings.config().appearance.input_style,
        InputStyle::Waterfall
    );
    assert!(settings.config().appearance.scroll_to_bottom_when_typing);
    assert_eq!(
        settings.config().appearance.window_backdrop,
        WindowBackdrop::Acrylic
    );
    assert_eq!(settings.config().profiles.list.len(), 1);
    assert_eq!(
        settings.config().profiles.default,
        settings.config().profiles.list[0].name
    );
    assert_eq!(settings.config().profiles.default, "PowerShell");
    assert!(settings.config().appearance.monospace_only);
    assert!(settings.config().system.restore_last_session_when_opening);
    assert_eq!(
        settings.config().appearance.smooth_scrolling,
        SmoothScrollingMode::All
    );
}

#[test]
fn default_profile_command_resolves_by_name() {
    let mut settings = AppSettings::from_config(Config {
        profiles: ProfilesConfig {
            list: vec![
                Profile {
                    name: "PowerShell".into(),
                    shell: default_shell_for_tests(),
                    args: String::new(),
                },
                Profile {
                    name: "Cmd".into(),
                    shell: "cmd.exe".into(),
                    args: "/k echo hi".into(),
                },
            ],
            default: "Cmd".into(),
        },
        ..Config::default()
    });

    let (shell, args) = settings.default_profile_command();

    assert_eq!(shell.as_deref(), Some("cmd.exe"));
    assert_eq!(args, vec!["/k", "echo", "hi"]);

    assert!(!settings.set_default_profile("Nope".into()));
    assert_eq!(settings.config().profiles.default, "Cmd");

    // An unknown name loaded from disk falls back to the first profile.
    let mut config = settings.config().clone();

    config.profiles.default = "Nope".into();
    settings = AppSettings::from_config(config);

    let (shell, _) = settings.default_profile_command();

    assert_eq!(shell.as_deref(), Some(default_shell_for_tests().as_str()));

    // Blank shell path: no override, session uses its built-in default.
    settings.set_profile_shell(0, "  ".into());
    settings.set_default_profile("PowerShell".into());

    let (shell, args) = settings.default_profile_command();

    assert!(shell.is_none());
    assert!(args.is_empty());
}

#[test]
fn profile_name_resolves_from_launch_command() {
    let mut settings = AppSettings::default();

    settings.add_profile();
    settings.rename_profile(1, "Developer PowerShell".into());
    settings.set_profile_shell(1, "pwsh.exe".into());
    settings.set_profile_args(1, "-NoLogo".into());

    assert_eq!(
        settings.profile_name_for_command(Some("PWSH.EXE"), &["-NoLogo".to_string()]),
        "Developer PowerShell"
    );
}

#[test]
fn profile_mutations_keep_default_valid() {
    let mut settings = AppSettings::default();

    // Add: unique placeholder names.
    settings.add_profile();
    settings.add_profile();

    assert_eq!(settings.config().profiles.list.len(), 3);
    assert_eq!(settings.config().profiles.list[1].name, "Profile 2");
    assert_eq!(settings.config().profiles.list[2].name, "Profile 3");

    // Rename the default: the reference follows.
    settings.rename_profile(0, "Pwsh".into());

    assert_eq!(settings.config().profiles.default, "Pwsh");

    // Remove the default: falls back to the first remaining profile.
    settings.remove_profile(0);

    assert_eq!(settings.config().profiles.default, "Profile 2");

    // The last profile cannot be removed.
    settings.remove_profile(0);
    settings.remove_profile(0);

    assert_eq!(settings.config().profiles.list.len(), 1);
}

#[test]
fn agent_profile_mutations_keep_default_valid() {
    let mut settings = AppSettings::default();

    // One seeded profile per registered harness, the first of which is the
    // default a new installation launches.
    assert_eq!(
        settings.config().agent_profiles.list.len(),
        AgentKind::ALL.len()
    );
    assert_eq!(settings.config().agent_profiles.default, "Claude Code");

    // Unique-name resolution: an empty desired name takes the kind
    // label, collisions get a numeric suffix, and the excluded index
    // (edit mode) keeps its own name available.
    assert_eq!(
        settings.unique_agent_profile_name("", AgentProfileKind::Claude, None),
        "Claude Code 2"
    );
    assert_eq!(
        settings.unique_agent_profile_name("Codex", AgentProfileKind::Codex, Some(1)),
        "Codex"
    );
    assert_eq!(
        settings.unique_agent_profile_name(" Mine ", AgentProfileKind::Codex, None),
        "Mine"
    );

    // Update with a rename: the default reference follows.
    let renamed = AgentProfile {
        name: "Proxy".into(),
        ..settings.config().agent_profiles.list[0].clone()
    };

    settings.save_agent_profile(Some(0), renamed);

    assert_eq!(settings.config().agent_profiles.default, "Proxy");

    // Remove the default: falls back to the first remaining profile.
    settings.remove_agent_profile(0);

    assert_eq!(settings.config().agent_profiles.default, "Codex");

    // Every profile can be removed; an empty list clears the default.
    while !settings.config().agent_profiles.list.is_empty() {
        settings.remove_agent_profile(0);
    }

    assert!(settings.config().agent_profiles.list.is_empty());
    assert_eq!(settings.config().agent_profiles.default, "");

    // The shortcut fallback still produces a launchable profile.
    assert_eq!(
        settings.default_agent_profile_entry().kind,
        AgentProfileKind::Claude
    );
}

#[test]
fn live_settings_edits_normalize_values_and_preserve_other_configuration() {
    let mut settings = AppSettings::from_config(Config {
        working_dir: Some("retained-directory".into()),
        ..Config::default()
    });

    settings.edit_appearance(|appearance| {
        appearance.terminal_font_family = "   ".into();
        appearance.terminal_font_size = f64::NAN;
        appearance.background_opacity = -1.0;
    });

    assert_eq!(
        settings.config().working_dir.as_deref(),
        Some("retained-directory")
    );
    assert_eq!(
        settings.config().appearance.terminal_font_family,
        DEFAULT_FONT_FAMILY
    );
    assert_eq!(
        settings.config().appearance.terminal_font_size,
        DEFAULT_FONT_SIZE
    );
    assert_eq!(settings.config().appearance.background_opacity, 0.2);
}

#[test]
fn profile_edits_keep_names_and_defaults_valid_across_reordering() {
    let mut settings = AppSettings::default();

    assert!(settings.duplicate_agent_profile(0));
    assert_eq!(
        settings.config().agent_profiles.list[1].name,
        "Claude Code 2"
    );
    assert!(settings.move_agent_profile(0, 2));
    assert_eq!(settings.config().agent_profiles.default, "Claude Code");
    assert_eq!(settings.default_agent_profile_entry().name, "Claude Code");

    let previous = settings.config().clone();

    assert!(!settings.move_agent_profile(99, 0));
    assert!(!settings.duplicate_agent_profile(99));
    assert!(!settings.save_agent_profile(Some(99), AgentProfile::default()));
    assert!(!settings.set_profile_shell(99, "missing".into()));
    assert_eq!(settings.config(), &previous);

    while !settings.config().agent_profiles.list.is_empty() {
        settings.remove_agent_profile(0);
    }

    assert!(settings.save_agent_profile(
        None,
        AgentProfile {
            name: "  Replacement  ".into(),
            env: vec![EnvVar {
                name: " ".into(),
                value: "unused".into()
            }],
            ..AgentProfile::default()
        }
    ));
    assert_eq!(settings.config().agent_profiles.default, "Replacement");
    assert!(settings.default_agent_profile_entry().env.is_empty());
}

#[test]
fn installation_update_titles_only_number_distinct_provider_installations() {
    assert_eq!(
        installation_update_title(ProviderKind::Claude, 1, 1),
        "Claude Code Updates"
    );
    assert_eq!(
        installation_update_title(ProviderKind::Codex, 2, 3),
        "Codex Updates 2"
    );
}

#[test]
fn unchecked_installations_do_not_render_unknown_versions() {
    assert_eq!(
        installation_version_text(UpdatePhase::Unknown, "unknown", "unknown"),
        "Not checked"
    );
    assert_eq!(
        installation_version_text(UpdatePhase::Available, "1.0.0", "1.1.0"),
        "1.0.0 → 1.1.0"
    );
}

#[test]
fn default_agent_profile_entry_resolves_by_name() {
    let mut settings = AppSettings::default();

    let profile = AgentProfile {
        executable: "custom-codex".into(),
        ..settings.config().agent_profiles.list[1].clone()
    };

    settings.save_agent_profile(Some(1), profile);
    settings.set_default_agent_profile("Codex".into());

    assert_eq!(
        settings.default_agent_profile_entry().executable,
        "custom-codex"
    );

    assert!(!settings.set_default_agent_profile("Nope".into()));
    assert_eq!(settings.config().agent_profiles.default, "Codex");

    // An unknown name loaded from disk falls back to the first profile.
    let mut config = settings.config().clone();

    config.agent_profiles.default = "Nope".into();
    settings = AppSettings::from_config(config);

    assert_eq!(
        settings.default_agent_profile_entry().kind,
        AgentProfileKind::Claude
    );
}

#[test]
fn defaults_have_one_powershell_profile() {
    let settings = AppSettings::default();

    assert_eq!(
        settings.config().appearance.input_style,
        InputStyle::Waterfall
    );
    assert!(settings.config().appearance.scroll_to_bottom_when_typing);
    assert_eq!(
        settings.config().appearance.window_backdrop,
        WindowBackdrop::Acrylic
    );
    assert_eq!(settings.config().profiles.list.len(), 1);
    assert!(
        settings.config().profiles.list[0].shell == default_shell_for_tests()
            || settings.config().profiles.list[0]
                .shell
                .ends_with(r"\pwsh.exe")
    );
    assert_eq!(settings.config().profiles.list[0].args, "");
    assert!(settings.config().system.restore_last_session_when_opening);
    assert_eq!(
        settings.config().appearance.smooth_scrolling,
        SmoothScrollingMode::All
    );
}

#[test]
fn failed_settings_save_keeps_edits_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    fs::write(&path, "invalid [ configuration").unwrap();

    let mut settings = AppSettings::default();

    assert!(settings.config().terminal.improve_powershell_compatibility);

    settings.edit_terminal(|section| section.improve_powershell_compatibility = false);

    settings.edit_appearance(|section| section.scroll_to_bottom_when_typing = false);
    settings.edit_appearance(|section| section.reduce_motion = true);
    settings.edit_appearance(|section| section.human_friendly_agent_ui_layout = false);
    settings.edit_appearance(|section| section.smooth_scrolling = SmoothScrollingMode::OnlyAgent);

    let error = settings.save_to(&path).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(settings.config().appearance.reduce_motion);
    assert!(!settings.config().appearance.scroll_to_bottom_when_typing);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "invalid [ configuration"
    );

    fs::write(&path, "# keep this\n[appearance]\nfuture-setting = 42\n").unwrap();
    settings.save_to(&path).unwrap();

    let saved = fs::read_to_string(&path).unwrap();

    assert!(saved.contains("# keep this"));
    assert!(saved.contains("future-setting = 42"));

    let config: Config = toml::from_str(&saved).unwrap();

    assert_eq!(config.appearance, settings.config().appearance);
    assert_eq!(config.agent, settings.config().agent);
    assert_eq!(config.system, settings.config().system);
    assert_eq!(config.update, settings.config().update);
    assert_eq!(config.remote_session, settings.config().remote_session);
    assert!(!config.terminal.improve_powershell_compatibility);
}

#[test]
fn settings_io_failure_preserves_edits_until_the_path_is_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    fs::create_dir(&path).unwrap();

    let mut settings = AppSettings::default();

    settings.edit_system(|section| section.confirm_before_closing_workspace = false);

    assert!(settings.save_to(&path).is_err());
    assert!(!settings.config().system.confirm_before_closing_workspace);
    assert!(path.is_dir());

    fs::remove_dir(&path).unwrap();
    settings.save_to(&path).unwrap();

    let config: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

    assert!(!config.system.confirm_before_closing_workspace);
}

fn list_pixel_position(state: &ListState) -> f32 {
    let offset = state.logical_scroll_top();

    offset.item_ix as f32 * 20. + offset.offset_in_item.as_f32()
}

struct SettingsAwareList(ListState);

impl gpui::Render for SettingsAwareList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.0.set_smooth_wheel_enabled(
            cx.global::<AppSettings>()
                .config()
                .appearance
                .smooth_scrolling
                .terminal_enabled(),
        );

        list(self.0.clone(), |_, _, _| {
            div().h(px(20.)).w_full().into_any_element()
        })
        .w_full()
        .h_full()
    }
}

fn draw_settings_aware_list(cx: &mut gpui::VisualTestContext, view: &Entity<SettingsAwareList>) {
    cx.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
        view.clone().into_any_element()
    });
}

#[gpui::test]
fn smooth_scrolling_mode_updates_an_open_terminal_list(cx: &mut TestAppContext) {
    cx.set_global(AppSettings::default());

    let state = ListState::new(50, ListAlignment::Top, px(10.)).measure_all();

    state.scroll_to(ListOffset {
        item_ix: 10,
        offset_in_item: px(0.),
    });

    let cx = cx.add_empty_window();
    let view = cx.update(|_, cx| cx.new(|_| SettingsAwareList(state.clone())));

    draw_settings_aware_list(cx, &view);

    cx.simulate_event(ScrollWheelEvent {
        position: point(px(1.), px(1.)),
        delta: ScrollDelta::Lines(point(0., 1.)),
        ..Default::default()
    });

    assert_eq!(list_pixel_position(&state), 200.);

    cx.executor().advance_clock(Duration::from_millis(100));
    draw_settings_aware_list(cx, &view);

    let stopped_at = list_pixel_position(&state);

    assert!(stopped_at > 150. && stopped_at < 200.);

    cx.update_global::<AppSettings, _>(|settings, _| {
        settings
            .edit_appearance(|section| section.smooth_scrolling = SmoothScrollingMode::OnlyAgent);
    });

    draw_settings_aware_list(cx, &view);
    cx.executor().advance_clock(Duration::from_millis(400));
    draw_settings_aware_list(cx, &view);

    assert!((list_pixel_position(&state) - stopped_at).abs() < 0.1);
}

#[test]
fn every_registered_harness_can_be_named_seeded_and_launched() {
    // A kind that is selectable in one surface and missing from another is
    // invisible in practice: the add dialog's picker, the seeded list, and the
    // built-in profile all have to agree on the same registry.
    for kind in AgentKind::ALL {
        let profile = builtin_agent_profile(kind);
        let id: &str = kind.into();

        assert_eq!(profile.kind, kind, "{}", id);
        assert!(!profile.executable.trim().is_empty(), "{}", id);
        assert!(!profile.name.trim().is_empty(), "{}", id);
        assert!(
            !agent_kind_display_label(kind).is_empty(),
            "{} has no display label",
            id
        );
    }

    // Round-tripping catches a conversion that quietly maps a new kind onto an
    // existing one, which would make its profiles open the wrong backend.
    for kind in AgentKind::ALL {
        assert_eq!(AgentKind::from_id(kind.into()), Some(kind));
    }

    assert_eq!(
        builtin_agent_profile(AgentProfileKind::DeepSeek).launcher,
        AgentProfileLauncher::Npx
    );
}

#[test]
fn built_in_ui_themes_parse_into_component_config() {
    for builtin in BUILTIN_THEMES {
        let theme: ConfigTheme = toml::from_str(builtin.source).unwrap();

        let ui = theme
            .ui_theme()
            .unwrap_or_else(|| panic!("{} has no [colors.ui] section", builtin.name));

        let config = ui_theme_config(&ui)
            .unwrap_or_else(|| panic!("{} has an unparsable [colors.ui] section", builtin.name));

        assert_eq!(config.name, ui.name);
        assert!(config.colors.background.is_some());
    }
}

/// Color names a theme file states under `[colors.ui]`. The corner radii share
/// that section in the file format while being a separate choice a theme may
/// leave to the application, and the syntax palette is a table of its own, so
/// neither is part of color coverage.
fn ui_color_names(name: &str) -> BTreeSet<String> {
    let theme: ConfigTheme = toml::from_str(builtin_theme_source(name).unwrap()).unwrap();

    theme
        .ui_theme()
        .unwrap()
        .colors
        .as_table()
        .unwrap()
        .keys()
        .filter(|key| {
            !matches!(
                key.as_str(),
                "radius" | "radius.lg" | "shadow" | "highlight"
            )
        })
        .map(ToString::to_string)
        .collect()
}

/// An unstated color is filled from the component library's own light or dark
/// palette, so a control the theme forgot renders in a foreign hue. Every
/// built-in names every color the library reads; comparing them against one
/// another is what catches a color added to one of them and missed on the
/// others, including after the library gains a new one.
#[test]
fn built_in_themes_state_the_same_colors() {
    let reference = ui_color_names("fluent_light");

    assert!(reference.len() > 100);

    for builtin in BUILTIN_THEMES {
        let name = builtin.name;
        let names = ui_color_names(name);
        let missing: Vec<_> = reference.difference(&names).collect();
        let extra: Vec<_> = names.difference(&reference).collect();

        assert!(
            missing.is_empty() && extra.is_empty(),
            "{name} misses {missing:?} and adds {extra:?}"
        );
    }
}

/// A theme that states no syntax palette falls back to the component
/// library's palette for its mode, which is tuned to the library's own
/// surfaces rather than the theme's. Every built-in therefore states a palette
/// of its own, and the palette's editor background sits on the same side of
/// mid-gray as the theme's mode so light colors never land on a light surface.
#[test]
fn built_in_themes_state_a_syntax_palette_for_their_mode() {
    for builtin in BUILTIN_THEMES {
        let name = builtin.name;
        let theme: ConfigTheme = toml::from_str(builtin.source).unwrap();
        let config = ui_theme_config(&theme.ui_theme().unwrap()).unwrap();

        let highlight = config
            .highlight
            .as_ref()
            .unwrap_or_else(|| panic!("{name} states no syntax palette"));

        let background = highlight
            .editor_background
            .unwrap_or_else(|| panic!("{name} states no editor background"));

        assert_eq!(background.l < 0.5, config.mode.is_dark(), "{name}");

        let syntax = &highlight.syntax;

        for (role, style) in [
            ("comment", &syntax.comment),
            ("keyword", &syntax.keyword),
            ("string", &syntax.string),
            ("type", &syntax.type_),
            ("number", &syntax.number),
        ] {
            assert!(style.is_some(), "{name} states no {role} color");
        }
    }
}

#[test]
fn fluent_themes_carry_their_own_corner_radii() {
    let theme: ConfigTheme = toml::from_str(builtin_theme_source("fluent_dark").unwrap()).unwrap();
    let config = ui_theme_config(&theme.ui_theme().unwrap()).unwrap();

    assert_eq!(config.radius, Some(4));
    assert_eq!(config.radius_lg, Some(8));
}
