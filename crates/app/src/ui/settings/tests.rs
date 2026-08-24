use std::time::Duration;

use gpui::{
    Context, Entity, IntoElement, ListAlignment, ListOffset, ListState, ScrollDelta,
    ScrollWheelEvent, TestAppContext, list, point, size,
};

use crate::agent::AgentKind;
use crate::ui::settings::*;

#[test]
fn cursor_shape_dropdown_values_match_config_shapes() {
    assert_eq!(cursor_shape_from_value("block"), CursorShape::Block);
    assert_eq!(cursor_shape_from_value("line"), CursorShape::Beam);
    assert_eq!(cursor_shape_from_value("underline"), CursorShape::Underline);
}

#[test]
fn tab_width_clamps_to_allowed_range() {
    assert_eq!(clamp_tab_width(DEFAULT_TAB_WIDTH), DEFAULT_TAB_WIDTH);
    assert_eq!(clamp_tab_width(200.0), 200.0);
    assert_eq!(clamp_tab_width(MAX_TAB_WIDTH), MAX_TAB_WIDTH);
    assert_eq!(clamp_tab_width(10.0), DEFAULT_TAB_WIDTH);
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

    assert_eq!(settings.agent_transcript_font_family, "Consolas");
    assert_eq!(
        settings.agent_transcript_font_size,
        DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE
    );
    assert_eq!(
        settings.appearance_config().agent_transcript_font_family,
        "Consolas"
    );
    assert_eq!(
        settings.appearance_config().agent_transcript_font_size,
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
        assert_eq!(WindowBackdrop::from_value(backdrop.as_str()), backdrop);
    }
    // Unknown values fall back to the opaque mode, which always renders.
    assert_eq!(WindowBackdrop::from_value("bogus"), WindowBackdrop::Off);
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
        assert_eq!(input_style_from_value(style.as_str()), style);
    }
    // Unknown values fall back to the default style.
    assert_eq!(input_style_from_value("bogus"), InputStyle::Waterfall);
}

#[test]
fn load_falls_back_to_default_profile() {
    // Test env has no config file: defaults apply, the empty profiles
    // list maps to the single built-in profile, and the unset default
    // profile resolves to that profile's name.
    let settings = AppSettings::load();
    assert_eq!(settings.input_style, InputStyle::Waterfall);
    assert!(settings.scroll_to_bottom_when_typing);
    assert_eq!(settings.window_backdrop, WindowBackdrop::Acrylic);
    assert_eq!(settings.profiles.len(), 1);
    assert_eq!(settings.default_profile, settings.profiles[0].name);
    assert_eq!(settings.default_profile, "PowerShell");
    assert!(settings.monospace_only);
    assert!(settings.restore_last_session_when_opening);
    assert_eq!(settings.smooth_scrolling, SmoothScrollingMode::All);
}

#[test]
fn default_profile_command_resolves_by_name() {
    let mut settings = AppSettings::default();
    settings.profiles = vec![
        Profile {
            name: "PowerShell".into(),
            shell: DEFAULT_SHELL.into(),
            args: String::new(),
        },
        Profile {
            name: "Cmd".into(),
            shell: "cmd.exe".into(),
            args: "/k echo hi".into(),
        },
    ];
    settings.default_profile = "Cmd".into();

    let (shell, args) = settings.default_profile_command();
    assert_eq!(shell.as_deref(), Some("cmd.exe"));
    assert_eq!(args, vec!["/k", "echo", "hi"]);

    // Dangling name falls back to the first profile.
    settings.default_profile = "Nope".into();
    let (shell, _) = settings.default_profile_command();
    assert_eq!(shell.as_deref(), Some(DEFAULT_SHELL));

    // Blank shell path: no override, session uses its built-in default.
    settings.profiles[0].shell = "  ".into();
    settings.default_profile = "PowerShell".into();
    let (shell, args) = settings.default_profile_command();
    assert!(shell.is_none());
    assert!(args.is_empty());
}

#[test]
fn profile_name_resolves_from_launch_command() {
    let mut settings = AppSettings::default();
    settings.profiles.push(Profile {
        name: "Developer PowerShell".into(),
        shell: "pwsh.exe".into(),
        args: "-NoLogo".into(),
    });

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
    assert_eq!(settings.profiles.len(), 3);
    assert_eq!(settings.profiles[1].name, "Profile 2");
    assert_eq!(settings.profiles[2].name, "Profile 3");

    // Rename the default: the reference follows.
    settings.rename_profile(0, "Pwsh".into());
    assert_eq!(settings.default_profile, "Pwsh");

    // Remove the default: falls back to the first remaining profile.
    settings.remove_profile(0);
    assert_eq!(settings.default_profile, "Profile 2");

    // The last profile cannot be removed.
    settings.remove_profile(0);
    settings.remove_profile(0);
    assert_eq!(settings.profiles.len(), 1);
}

#[test]
fn agent_profile_mutations_keep_default_valid() {
    let mut settings = AppSettings::default();
    // One seeded profile per registered harness, the first of which is the
    // default a new installation launches.
    assert_eq!(settings.agent_profiles.len(), AgentKind::ALL.len());
    assert_eq!(settings.default_agent_profile, "Claude Code");

    // Unique-name resolution: an empty desired name takes the kind
    // label, collisions get a numeric suffix, and the excluded index
    // (edit mode) keeps its own name available.
    assert_eq!(
        settings.unique_agent_profile_name("", AgentProfileKind::ClaudeCode, None),
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
        ..settings.agent_profiles[0].clone()
    };
    settings.update_agent_profile(0, renamed);
    assert_eq!(settings.default_agent_profile, "Proxy");

    // Remove the default: falls back to the first remaining profile.
    settings.remove_agent_profile(0);
    assert_eq!(settings.default_agent_profile, "Codex");

    // Every profile can be removed; an empty list clears the default.
    while !settings.agent_profiles.is_empty() {
        settings.remove_agent_profile(0);
    }
    assert!(settings.agent_profiles.is_empty());
    assert_eq!(settings.default_agent_profile, "");

    // The shortcut fallback still produces a launchable profile.
    assert_eq!(
        settings.default_agent_profile_entry().kind,
        AgentProfileKind::ClaudeCode
    );
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
    settings.agent_profiles[1].executable = "custom-codex".into();
    settings.default_agent_profile = "Codex".into();
    assert_eq!(
        settings.default_agent_profile_entry().executable,
        "custom-codex"
    );

    // Dangling name falls back to the first profile.
    settings.default_agent_profile = "Nope".into();
    assert_eq!(
        settings.default_agent_profile_entry().kind,
        AgentProfileKind::ClaudeCode
    );
}

#[test]
fn defaults_have_one_powershell_profile() {
    let settings = AppSettings::default();
    assert_eq!(settings.input_style, InputStyle::Waterfall);
    assert!(settings.scroll_to_bottom_when_typing);
    assert_eq!(settings.window_backdrop, WindowBackdrop::Acrylic);
    assert_eq!(settings.profiles.len(), 1);
    assert!(
        settings.profiles[0].shell == DEFAULT_SHELL
            || settings.profiles[0].shell.ends_with(r"\pwsh.exe")
    );
    assert_eq!(settings.profiles[0].args, "");
    assert!(settings.restore_last_session_when_opening);
    assert_eq!(settings.smooth_scrolling, SmoothScrollingMode::All);
}

#[test]
fn scroll_to_bottom_when_typing_maps_to_saved_appearance() {
    let mut settings = AppSettings::default();
    settings.scroll_to_bottom_when_typing = false;

    assert!(!settings.appearance_config().scroll_to_bottom_when_typing);
}

#[test]
fn smooth_scrolling_maps_to_saved_appearance() {
    let mut settings = AppSettings::default();
    for mode in [
        SmoothScrollingMode::All,
        SmoothScrollingMode::OnlyTerminal,
        SmoothScrollingMode::OnlyAgent,
        SmoothScrollingMode::Off,
    ] {
        settings.smooth_scrolling = mode;
        assert_eq!(settings.appearance_config().smooth_scrolling, mode);
    }
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
        settings.smooth_scrolling = SmoothScrollingMode::OnlyAgent;
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
        let profile = builtin_agent_profile(kind.profile_kind());

        assert_eq!(profile.kind, kind.profile_kind(), "{}", kind.id());
        assert!(!profile.executable.trim().is_empty(), "{}", kind.id());
        assert!(!profile.name.trim().is_empty(), "{}", kind.id());
        assert!(
            !agent_kind_display_label(kind.profile_kind()).is_empty(),
            "{} has no display label",
            kind.id()
        );
    }

    // Round-tripping catches a conversion that quietly maps a new kind onto an
    // existing one, which would make its profiles open the wrong backend.
    for kind in AgentKind::ALL {
        assert_eq!(AgentKind::from_profile(kind.profile_kind()), kind);
        assert_eq!(AgentKind::from_id(kind.id()), Some(kind));
    }
}
