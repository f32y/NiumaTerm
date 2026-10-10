use tempfile::Builder as TempDirBuilder;

use crate::application::*;

#[test]
fn missing_newline_setting_uses_shift_enter_and_preserves_saved_choices() {
    for text in ["", "[system]\n"] {
        let config: Config = parse_toml(text).unwrap();

        assert_eq!(
            config.system.newline_shortcut,
            system::NewlineShortcut::ShiftEnter
        );
    }

    for (value, expected) in [
        ("shift-enter", system::NewlineShortcut::ShiftEnter),
        ("ctrl-enter", system::NewlineShortcut::CtrlEnter),
        ("off", system::NewlineShortcut::Off),
    ] {
        let config: Config =
            parse_toml(&format!("[system]\nnewline-shortcut = \"{value}\"\n")).unwrap();

        let saved = toml::to_string(&config).unwrap();
        let restored: Config = parse_toml(&saved).unwrap();

        assert_eq!(restored.system.newline_shortcut, expected);
    }
}

#[test]
fn example_system_section_matches_defaults_on_every_platform() {
    let shipped =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(EXAMPLE_CONFIG_PATH))
            .unwrap();

    let example: toml::Value = parse_toml(&shipped).unwrap();
    let defaults = toml::Value::try_from(SystemConfig::default()).unwrap();

    assert_eq!(example.get("system"), Some(&defaults));
}

#[test]
fn powershell_compatibility_defaults_on_for_existing_configs_and_preserves_opt_out() {
    for text in ["", "[terminal]\n", "[appearance]\n"] {
        let config: Config = parse_toml(text).unwrap();

        assert!(config.terminal.improve_powershell_compatibility);
    }

    let config: Config =
        parse_toml("[terminal]\nimprove-powershell-compatibility = false\n").unwrap();

    let saved = toml::to_string(&config).unwrap();
    let restored: Config = parse_toml(&saved).unwrap();

    assert!(!restored.terminal.improve_powershell_compatibility);
}

fn sample_appearance() -> AppearanceConfig {
    AppearanceConfig {
        input_style: appearance::InputStyle::Waterfall,
        scroll_to_bottom_when_typing: false,
        agent_pane_use_terminal_background: true,
        command_blocks: false,
        show_daily_token_usage: true,
        show_git_status_on_title_bar: true,
        git_status_refresh_interval: 15,
        tab_bar_style: appearance::TabBarStyle::Vertical,
        ui_font: "Arial".to_string(),
        terminal_font_family: "Cascadia Code".to_string(),
        terminal_font_size: 16.0,
        terminal_line_height: 1.2,
        agent_font_family: "Cascadia Code".to_string(),
        agent_font_size: 15.0,
        monospace_only: false,
        window_backdrop: appearance::WindowBackdrop::Acrylic,
        transparent_main_view: false,
        smooth_scrolling: appearance::SmoothScrollingMode::Off,
        background_opacity: 0.85,
        background_image: Some(r"C:\Wallpapers\background.png".to_string()),
        background_image_opacity: 0.4,
        language: appearance::Language::ZhCn,
        agent_transcript_font_family: "JetBrains Mono".to_string(),
        agent_transcript_font_size: 12.5,
        reduce_motion: true,
        human_friendly_agent_ui_layout: false,
        tab_shape: appearance::TabShape::Rounded,
    }
}

fn sample_system() -> SystemConfig {
    SystemConfig {
        restore_last_session_when_opening: false,
        warn_before_terminating_shell: system::WarnBeforeTerminatingShell::Disabled,
        confirm_before_closing_workspace: false,
        prioritize_ui_threads: true,
        newline_shortcut: system::NewlineShortcut::ShiftEnter,
        open_in_best_workspace: false,
        send_system_notifications: false,
        proxy: system::ProxyMode::Socks,
        proxy_url: "127.0.0.1:7890".into(),
    }
}

fn sample_agent() -> AgentConfig {
    AgentConfig {
        enable_agent_hooks: false,
        show_agent_usage: false,
        collapse_tool_calls: agent::CollapseRows::WorkAndToolCalls,
        check_agent_updates: false,
        codex_skill_command_compat: false,
        model_list_style: agent::ModelListStyle::IdOnly,
        enable_agent_team: true,
        token_speed_mode: agent::TokenSpeedMode::Session,
        answer_questions_one_at_a_time: true,
        unified_agent_tab: false,
        enable_agent_orchestration: true,
    }
}

fn sample_profiles() -> Vec<Profile> {
    vec![Profile {
        name: "PowerShell".to_string(),
        shell: r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe".to_string(),
        args: "-NoLogo".to_string(),
    }]
}

fn sample_agent_profiles() -> Vec<profile::AgentProfile> {
    vec![profile::AgentProfile {
        name: "Claude Code".to_string(),
        kind: profile::AgentKind::Claude,
        executable: "claude".to_string(),
        launcher: profile::AgentProfileLauncher::Custom,
        model: "claude-opus-4-8".to_string(),
        effort: "high".to_string(),
        replace_sub_models: true,
        use_custom_endpoint: true,
        cache_warn_minutes: 30,
        api_base_url: "https://proxy.example.com".to_string(),
        api_key: "sk-test".to_string(),
        env: vec![profile::EnvVar {
            name: "FOO".to_string(),
            value: "bar".to_string(),
        }],
        vision_model: false,
        approval: "acceptEdits".to_string(),
        sandbox: String::new(),
        dsh_version: profile::DshVersion::V0_2_0,
    }]
}

fn patch_settings(doc: &mut DocumentMut) {
    patch_settings_document(
        doc,
        &SettingsPatch {
            theme: "test-theme",
            appearance: &sample_appearance(),
            cursor_shape: CursorShape::Beam,
            agent: &sample_agent(),
            system: &sample_system(),
            update: &update::UpdateConfig::default(),
            profiles: &sample_profiles(),
            default_profile: "PowerShell",
            agent_profiles: &sample_agent_profiles(),
            default_agent_profile: "Claude Code",
            terminal: &TerminalConfig::default(),
            remote: &RemoteConfig::default(),
        },
    )
    .unwrap();
}

#[test]
fn settings_patch_preserves_comments_and_unrelated_keys() {
    let existing = "# my terminal config\ntheme = \"dark\"\n\n[window]\nwidth = 960\n\n[appearance]\n# Keep my font note\nterminal-font-size = 14.0 # reading size\n";

    let mut doc = existing.parse::<DocumentMut>().unwrap();

    patch_settings(&mut doc);

    let out = doc.to_string();

    assert!(out.contains("# my terminal config"));
    assert!(out.contains("width = 960"));
    assert!(out.contains("# Keep my font note"));
    assert!(out.contains("terminal-font-size = 16.0 # reading size"));
    assert!(out.contains("smooth-scrolling = \"off\""));
    assert!(out.contains("agent-transcript-font-family = \"JetBrains Mono\""));
    assert!(out.contains("agent-transcript-font-size = 12.5"));
    assert!(out.contains("reduce-motion = true"));
    assert!(out.contains("human-friendly-agent-ui-layout = false"));

    let config: Config = parse_toml(&out).unwrap();

    assert_eq!(config.appearance, sample_appearance());
    assert_eq!(config.agent, sample_agent());
    assert_eq!(config.system, sample_system());
    assert_eq!(config.profiles.list, sample_profiles());
    assert_eq!(config.profiles.default, "PowerShell");
    assert_eq!(config.agent_profiles.list, sample_agent_profiles());
    assert_eq!(config.agent_profiles.default, "Claude Code");
    assert!(config.agent_profiles.initialized);
    assert_eq!(config.cursor.shape, CursorShape::Beam);
}

#[test]
fn settings_patch_converts_inline_tables() {
    let mut doc =
        "fonts = { size = 12.0, hinting = true }\nappearance = { monospace-only = false }\n"
            .parse::<DocumentMut>()
            .unwrap();

    patch_settings(&mut doc);

    let out = doc.to_string();

    assert!(out.contains("fonts = { size = 12.0, hinting = true }"));

    let config: Config = parse_toml(&out).unwrap();

    assert_eq!(config.appearance, sample_appearance());
}

#[test]
fn settings_patch_preserves_unknown_group_keys_and_removes_cleared_image() {
    let mut doc = r#"# Keep user comments
appearance = { future-appearance = 42, background-image = "old.png" }
agent = { future-agent = "keep" }
system = { future-system = true }
update = { future-update = "keep" }
"#
    .parse::<DocumentMut>()
    .unwrap();

    let appearance = AppearanceConfig {
        background_image: None,
        ..sample_appearance()
    };

    patch_settings_document(
        &mut doc,
        &SettingsPatch {
            theme: "test-theme",
            appearance: &appearance,
            cursor_shape: CursorShape::Beam,
            agent: &sample_agent(),
            system: &sample_system(),
            update: &update::UpdateConfig::default(),
            profiles: &sample_profiles(),
            default_profile: "PowerShell",
            agent_profiles: &sample_agent_profiles(),
            default_agent_profile: "Claude Code",
            terminal: &TerminalConfig::default(),
            remote: &RemoteConfig::default(),
        },
    )
    .unwrap();

    assert_eq!(
        doc["appearance"]["future-appearance"].as_integer(),
        Some(42)
    );
    assert_eq!(doc["agent"]["future-agent"].as_str(), Some("keep"));
    assert_eq!(doc["system"]["future-system"].as_bool(), Some(true));
    assert_eq!(doc["update"]["future-update"].as_str(), Some("keep"));
    assert!(
        doc["appearance"]
            .as_table()
            .unwrap()
            .get("background-image")
            .is_none()
    );

    let saved = doc.to_string();

    assert!(saved.contains("# Keep user comments"));

    let reloaded: Config = parse_toml(&saved).unwrap();

    assert_eq!(reloaded.appearance, appearance);
    assert_eq!(reloaded.agent, sample_agent());
    assert_eq!(reloaded.system, sample_system());
}

#[test]
fn save_settings_to_creates_updates_and_rejects_invalid() {
    let dir = env::temp_dir().join("NiumaTerm-settings-save-test");
    let _ = fs::remove_dir_all(&dir);
    let path = dir.join("config.toml");

    let save = || {
        save_settings_to(
            &path,
            &SettingsPatch {
                theme: "test-theme",
                appearance: &sample_appearance(),
                cursor_shape: CursorShape::Beam,
                agent: &sample_agent(),
                system: &sample_system(),
                update: &update::UpdateConfig::default(),
                profiles: &sample_profiles(),
                default_profile: "PowerShell",
                agent_profiles: &sample_agent_profiles(),
                default_agent_profile: "Claude Code",
                terminal: &TerminalConfig::default(),
                remote: &RemoteConfig::default(),
            },
        )
    };

    save().unwrap();

    let config: Config = parse_toml(&fs::read_to_string(&path).unwrap()).unwrap();

    assert_eq!(config.appearance, sample_appearance());
    assert_eq!(config.agent, sample_agent());
    assert_eq!(config.theme, "test-theme");

    save().unwrap();

    let config: Config = parse_toml(&fs::read_to_string(&path).unwrap()).unwrap();

    assert_eq!(config.profiles.default, "PowerShell");
    assert!(!path.with_extension("toml.tmp").exists());

    fs::write(&path, [0xff, 0xfe]).unwrap();

    assert_eq!(save().unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&path).unwrap(), [0xff, 0xfe]);

    fs::write(&path, "not [ valid").unwrap();

    assert!(save().is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "not [ valid");

    let _ = fs::remove_dir_all(&dir);
}

/// The stored `api-credentials` string of the first agent profile.
fn stored_credentials(doc: &DocumentMut) -> String {
    doc["agent-profiles"]["list"]
        .as_array_of_tables()
        .unwrap()
        .get(0)
        .unwrap()["api-credentials"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn saved_agent_credentials_contain_no_plaintext() {
    let mut doc = DocumentMut::new();

    patch_settings(&mut doc);

    let out = doc.to_string();

    assert!(out.contains("api-credentials = \"aes256gcm-v1:"));
    assert!(!out.contains("proxy.example.com"));
    assert!(!out.contains("sk-test"));
    assert!(!out.contains("api-base-url"));
    assert!(!out.contains("api-key"));

    let config: Config = parse_toml(&out).unwrap();

    assert_eq!(config.agent_profiles.list, sample_agent_profiles());
}

#[test]
fn repeated_saves_produce_different_stored_credentials() {
    let mut first = DocumentMut::new();

    patch_settings(&mut first);

    let mut second = DocumentMut::new();

    patch_settings(&mut second);

    assert_ne!(stored_credentials(&first), stored_credentials(&second));

    let restored: Config = parse_toml(&second.to_string()).unwrap();

    assert_eq!(restored.agent_profiles.list, sample_agent_profiles());
}

#[test]
fn empty_agent_credentials_are_omitted() {
    let profiles = vec![profile::AgentProfile {
        name: "Plain".to_string(),
        ..profile::AgentProfile::default()
    }];

    let mut doc = DocumentMut::new();

    profile::patch_agent_table(
        ensure_explicit_table(&mut doc, "agent-profiles"),
        &profiles,
        "Plain",
    )
    .unwrap();

    let out = doc.to_string();

    assert!(!out.contains("api-credentials"));
    assert!(!out.contains("api-base-url"));
    assert!(!out.contains("api-key"));
}

#[test]
fn legacy_npx_launcher_loads_and_migrates_to_the_launcher_enum() {
    let legacy = r#"
[[agent-profiles.list]]
name = "DeepSeek Harness"
kind = "deepseek"
executable = "dsh"
via-npx = true
"#;

    let config: Config = parse_toml(legacy).unwrap();
    let loaded = &config.agent_profiles.list[0];

    assert_eq!(loaded.launcher, profile::AgentProfileLauncher::Npx);

    let mut doc = legacy.parse::<DocumentMut>().unwrap();

    profile::patch_agent_table(
        ensure_explicit_table(&mut doc, "agent-profiles"),
        &config.agent_profiles.list,
        "DeepSeek Harness",
    )
    .unwrap();

    let saved = doc.to_string();

    assert!(saved.contains("launcher = \"npx\""));
    assert!(!saved.contains("via-npx"));
}

#[test]
fn pnpm_dlx_launcher_round_trips() {
    let source = r#"
[[agent-profiles.list]]
name = "DeepSeek Harness"
kind = "deepseek"
executable = "dsh"
launcher = "pnpm-dlx"
"#;

    let config: Config = parse_toml(source).unwrap();

    assert_eq!(
        config.agent_profiles.list[0].launcher,
        profile::AgentProfileLauncher::PnpmDlx
    );

    let mut doc = DocumentMut::new();

    profile::patch_agent_table(
        ensure_explicit_table(&mut doc, "agent-profiles"),
        &config.agent_profiles.list,
        "DeepSeek Harness",
    )
    .unwrap();

    let restored: Config = parse_toml(&doc.to_string()).unwrap();

    assert_eq!(restored.agent_profiles.list, config.agent_profiles.list);
}

#[test]
fn a_profile_saved_before_releases_were_selectable_moves_to_the_newest_one() {
    let source = r#"
[[agent-profiles.list]]
name = "DeepSeek Harness"
kind = "deepseek"
launcher = "npx"
"#;

    let config: Config = parse_toml(source).unwrap();

    assert_eq!(
        config.agent_profiles.list[0].dsh_version,
        profile::DshVersion::V0_2_0
    );

    let mut older = config.agent_profiles.list.clone();

    older[0].dsh_version = profile::DshVersion::V0_1_5;

    let mut doc = DocumentMut::new();

    profile::patch_agent_table(
        ensure_explicit_table(&mut doc, "agent-profiles"),
        &older,
        "DeepSeek Harness",
    )
    .unwrap();

    let saved = doc.to_string();

    assert!(saved.contains("dsh-version = \"0.1.5-rc.1\""));

    let restored: Config = parse_toml(&saved).unwrap();

    assert_eq!(restored.agent_profiles.list, older);
}

const LEGACY_PROFILE_TOML: &str = r#"
[[agent-profiles.list]]
name = "Legacy"
kind = "claude-code"
executable = "claude"
use-custom-endpoint = true
api-base-url = "https://legacy.example.com"
api-key = "sk-legacy"
"#;

#[test]
fn legacy_plaintext_credentials_load_without_touching_the_file() {
    let dir = TempDirBuilder::new()
        .prefix("NiumaTerm-legacy-credentials-test")
        .tempdir()
        .unwrap();

    let path = dir.path().join("config.toml");

    fs::write(&path, LEGACY_PROFILE_TOML).unwrap();

    let config = Config::load_for_startup_from(&path, dir.path()).unwrap();
    let profile = &config.agent_profiles.list[0];

    assert_eq!(profile.api_base_url, "https://legacy.example.com");
    assert_eq!(profile.api_key, "sk-legacy");
    assert_eq!(fs::read_to_string(&path).unwrap(), LEGACY_PROFILE_TOML);
}

#[test]
fn legacy_plaintext_credentials_migrate_on_save() {
    let config: Config = parse_toml(LEGACY_PROFILE_TOML).unwrap();

    let mut doc = LEGACY_PROFILE_TOML.parse::<DocumentMut>().unwrap();

    profile::patch_agent_table(
        ensure_explicit_table(&mut doc, "agent-profiles"),
        &config.agent_profiles.list,
        "Legacy",
    )
    .unwrap();

    let out = doc.to_string();

    assert!(out.contains("api-credentials = \"aes256gcm-v1:"));
    assert!(!out.contains("api-base-url"));
    assert!(!out.contains("api-key"));
    assert!(!out.contains("sk-legacy"));

    let restored: Config = parse_toml(&out).unwrap();
    let profile = &restored.agent_profiles.list[0];

    assert_eq!(profile.api_base_url, "https://legacy.example.com");
    assert_eq!(profile.api_key, "sk-legacy");
}

#[test]
fn encrypted_credentials_win_over_adjacent_legacy_fields() {
    let stored = encrypt_credentials("https://current.example.com", "sk-current").unwrap();

    let toml_str = format!(
        "[[agent-profiles.list]]\nname = \"Both\"\napi-credentials = \"{stored}\"\n\
             api-base-url = \"https://stale.example.com\"\napi-key = \"sk-stale\"\n"
    );

    let config: Config = parse_toml(&toml_str).unwrap();
    let profile = &config.agent_profiles.list[0];

    assert_eq!(profile.api_base_url, "https://current.example.com");
    assert_eq!(profile.api_key, "sk-current");
}

#[test]
fn invalid_encrypted_credentials_fail_without_legacy_fallback() {
    let valid = encrypt_credentials("https://real.example.com", "sk-real").unwrap();

    // Corrupt the last Base64 character while keeping the text decodable.
    let mut modified = valid.clone();

    let last = modified.pop().unwrap();

    modified.push(if last == 'A' { 'B' } else { 'A' });

    for bad in [
        "aes256gcm-v1:@@not-base64@@".to_string(),
        "aes256gcm-v9:AAAA".to_string(),
        modified,
    ] {
        let toml_str = format!(
            "[[agent-profiles.list]]\nname = \"Broken\"\napi-credentials = \"{bad}\"\n\
                 api-base-url = \"https://stale.example.com\"\napi-key = \"sk-stale\"\n"
        );

        let err = parse_toml::<Config>(&toml_str).unwrap_err().to_string();

        assert!(err.contains("Broken"), "{err}");
        assert!(!err.contains("sk-real"), "{err}");
        assert!(!err.contains("sk-stale"), "{err}");

        let payload = bad.strip_prefix("aes256gcm-").unwrap_or(&bad);

        assert!(!err.contains(payload), "{err}");
    }
}

#[test]
fn testing_mode_uses_test_subdirectory() {
    let base: PathBuf = "NiumaTerm".into();

    assert_eq!(config_dir_for_mode(base.clone(), false), base);
    assert_eq!(config_dir_for_mode(base.clone(), true), base.join("Test"));
}

fn create_temporary_config(prefix: &str, toml_str: &str) -> Config {
    let dir = TempDirBuilder::new().prefix(prefix).tempdir().unwrap();
    let path = dir.path().join("config.toml");

    fs::write(&path, toml_str).unwrap();

    Config::load_for_startup_from(&path, dir.path()).unwrap()
}

/// Terminal palette of the built-in default theme, which a config that
/// doesn't name a theme resolves to.
#[test]
fn startup_load_defaults_when_missing_and_errors_on_bad_toml() {
    let dir = TempDirBuilder::new()
        .prefix("NiumaTerm-startup-config-test")
        .tempdir()
        .unwrap();

    let path = dir.path().join("config.toml");

    let missing = Config::load_for_startup_from(&path, dir.path()).unwrap();

    assert_eq!(missing, Config::default());

    fs::write(&path, "not [ valid").unwrap();

    assert!(Config::load_for_startup_from(&path, dir.path()).is_err());

    fs::write(&path, [0xff]).unwrap();

    assert!(Config::load_for_startup_from(&path, dir.path()).is_err());

    assert!(Config::load_for_startup_from(dir.path(), dir.path()).is_err());
}

#[test]
fn theme_list_loads_valid_toml_files_in_name_order() {
    let temp = TempDirBuilder::new()
        .prefix("NiumaTerm-theme-list-test")
        .tempdir()
        .unwrap();

    let dir = temp.path();

    fs::write(
        dir.join("Zulu.toml"),
        "[colors.terminal]\nbackground = '#111111'\n",
    )
    .unwrap();

    fs::write(
        dir.join("alpha.toml"),
        "[colors.terminal]\nbackground = '#222222'\n",
    )
    .unwrap();

    fs::write(dir.join("invalid.toml"), "[colors\n").unwrap();

    fs::write(dir.join("ignored.txt"), "[colors.terminal]\n").unwrap();

    let themes = Config::load_themes_from(dir);

    assert_eq!(
        themes
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "Zulu"]
    );
}

#[test]
fn copied_builtin_keeps_family_without_changing_customized_colors() {
    let dir = TempDirBuilder::new()
        .prefix("theme-family-upgrade")
        .tempdir()
        .unwrap();

    let source = get_builtin_theme("slate_light")
        .unwrap()
        .replace("family = \"Slate\"\n", "")
        .replace("#FCFDFE", "#FAFAFA");

    let path = dir.path().join("slate_light.toml");

    fs::write(&path, source).unwrap();

    let theme = Config::load_theme(&path).unwrap();

    assert_eq!(theme.family, "Slate");
    assert_eq!(
        theme.colors.terminal.background,
        [250.0 / 255.0, 250.0 / 255.0, 250.0 / 255.0, 1.0]
    );
}

#[test]
fn top_level_colors_are_ignored() {
    let result = create_temporary_config(
        "ignored-colors",
        r#"
            theme = ""

            [colors]
            background = '#2B3E50'
        "#,
    );

    assert_eq!(result.colors, Colors::default());
}

const EXAMPLE_CONFIG_PATH: &str = "../../assets/config-example.toml";

/// `assets/config-example.toml` documents every key with its built-in
/// default. Nothing regenerates it, so this compares it against the real
/// serialized default: a key added, removed, or renamed on `Config` fails
/// here instead of leaving the example advertising settings that no longer
/// exist. Run with `--nocapture` to print the replacement content.
///
/// The shipped file records the Windows defaults (shell, editor and font
/// values differ per platform), so only that host can hold it to them. A key
/// added anywhere still fails there, and this test guards exactly that.
#[cfg(target_os = "windows")]
#[test]
fn example_config_matches_the_serialized_defaults() {
    let generated = toml::to_string_pretty(&Config::default()).expect("defaults serialize");

    let shipped =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(EXAMPLE_CONFIG_PATH))
            .expect("example config is readable");

    let body = shipped
        .split_once("\n\n")
        .map(|(_, body)| body)
        .unwrap_or(shipped.as_str());

    if body.trim() != generated.trim() {
        println!("---- regenerated assets/config-example.toml body ----");
        println!("{generated}");
    }

    assert_eq!(
        body.trim(),
        generated.trim(),
        "assets/config-example.toml is out of date"
    );
}

fn encrypt_credentials(api_base_url: &str, api_key: &str) -> Result<String, String> {
    let mut table = Table::new();

    let profile = profile::AgentProfile {
        api_base_url: api_base_url.into(),
        api_key: api_key.into(),
        ..profile::AgentProfile::default()
    };

    profile::patch_agent_table(&mut table, &[profile], "")?;

    Ok(table["list"][0]["api-credentials"]
        .as_str()
        .unwrap()
        .to_owned())
}

#[test]
fn startup_load_normalizes_appearance_before_any_ui_reads_it() {
    let config = create_temporary_config(
        "invalid-appearance",
        r#"
[appearance]
ui-font = " "
terminal-font-family = ""
agent-font-family = " "
agent-transcript-font-family = ""
terminal-font-size = -1
agent-font-size = 500
agent-transcript-font-size = nan
terminal-line-height = inf
background-opacity = -5
background-image-opacity = nan
background-image = " "
git-status-refresh-interval = 1
"#,
    );

    let appearance = &config.appearance;
    let defaults = AppearanceConfig::default();

    assert_eq!(appearance.ui_font, defaults.ui_font);
    assert_eq!(
        appearance.terminal_font_family,
        defaults.terminal_font_family
    );
    assert_eq!(appearance.agent_font_family, defaults.agent_font_family);
    assert_eq!(
        appearance.agent_transcript_font_family,
        defaults.agent_transcript_font_family
    );
    assert_eq!(appearance.terminal_font_size, 6.0);
    assert_eq!(appearance.agent_font_size, 72.0);
    assert_eq!(
        appearance.agent_transcript_font_size,
        defaults.agent_transcript_font_size
    );
    assert_eq!(
        appearance.terminal_line_height,
        defaults.terminal_line_height
    );
    assert_eq!(appearance.background_opacity, 0.2);
    assert_eq!(
        appearance.background_image_opacity,
        defaults.background_image_opacity
    );
    assert_eq!(appearance.background_image, None);
    assert_eq!(appearance.git_status_refresh_interval, 30);
}

#[test]
fn every_platform_default_font_loads_as_this_platform_default() {
    let defaults = AppearanceConfig::default();

    for (ui, mono) in [("Segoe UI", "Consolas"), (".SystemUIFont", "Menlo")] {
        let config = create_temporary_config(
            "platform-default-fonts",
            &format!(
                r#"
[appearance]
ui-font = "{ui}"
terminal-font-family = "{mono}"
agent-font-family = "{ui}"
agent-transcript-font-family = "{mono}"
"#
            ),
        );

        let appearance = &config.appearance;

        assert_eq!(appearance.ui_font, defaults.ui_font);
        assert_eq!(
            appearance.terminal_font_family,
            defaults.terminal_font_family
        );
        assert_eq!(appearance.agent_font_family, defaults.agent_font_family);
        assert_eq!(
            appearance.agent_transcript_font_family,
            defaults.agent_transcript_font_family
        );
    }
}

#[test]
fn older_system_settings_keep_native_notifications_enabled() {
    let config: Config = parse_toml("[system]\nopen-in-best-workspace = false\n").unwrap();

    assert!(config.system.send_system_notifications);
    assert!(!config.system.open_in_best_workspace);
}
