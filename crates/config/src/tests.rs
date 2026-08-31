use std::io::Write;

use colors::hex_to_color_arr;

use crate::*;

fn sample_appearance() -> AppearanceConfig {
    AppearanceConfig {
        input_style: appearance::InputStyle::Waterfall,
        scroll_to_bottom_when_typing: false,
        agent_pane_use_terminal_background: true,
        command_blocks: false,
        show_daily_token_usage: true,
        show_git_status_on_title_bar: true,
        git_status_refresh_interval: 15,
        tab_width: 150.0,
        tab_auto_size: true,
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
    }
}

fn sample_system() -> SystemConfig {
    SystemConfig {
        restore_last_session_when_opening: false,
        manage_subprocess_job: true,
        warn_before_terminating_shell: system::WarnBeforeTerminatingShell::Disabled,
        confirm_before_closing_workspace: false,
        prioritize_ui_threads: true,
        newline_shortcut: system::NewlineShortcut::ShiftEnter,
        open_in_best_workspace: false,
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
        kind: profile::AgentProfileKind::ClaudeCode,
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
            remote_session: &remote_session::RemoteSessionConfig::default(),
            update: &update::UpdateConfig::default(),
            profiles: &sample_profiles(),
            default_profile: "PowerShell",
            agent_profiles: &sample_agent_profiles(),
            default_agent_profile: "Claude Code",
        },
    )
    .unwrap();
}

#[test]
fn settings_patch_preserves_comments_and_unrelated_keys() {
    let existing = "# my terminal config\ntheme = \"dark\"\n\n[window]\nwidth = 960\n";
    let mut doc = existing.parse::<DocumentMut>().unwrap();

    patch_settings(&mut doc);
    let out = doc.to_string();

    assert!(out.contains("# my terminal config"));
    assert!(out.contains("width = 960"));
    assert!(out.contains("smooth-scrolling = \"off\""));
    assert!(out.contains("agent-transcript-font-family = \"JetBrains Mono\""));
    assert!(out.contains("agent-transcript-font-size = 12.5"));
    assert!(out.contains("reduce-motion = true"));

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
                remote_session: &remote_session::RemoteSessionConfig::default(),
                update: &update::UpdateConfig::default(),
                profiles: &sample_profiles(),
                default_profile: "PowerShell",
                agent_profiles: &sample_agent_profiles(),
                default_agent_profile: "Claude Code",
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
    profile::patch_agent_document(&mut doc, &profiles, "Plain").unwrap();
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
    profile::patch_agent_document(&mut doc, &config.agent_profiles.list, "DeepSeek Harness")
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
    profile::patch_agent_document(&mut doc, &config.agent_profiles.list, "DeepSeek Harness")
        .unwrap();
    let restored: Config = parse_toml(&doc.to_string()).unwrap();

    assert_eq!(restored.agent_profiles.list, config.agent_profiles.list);
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
    let dir = tmp_dir().join("NiumaTerm-legacy-credentials-test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(&path, LEGACY_PROFILE_TOML).unwrap();

    let config = Config::load_for_startup_from(&path, &dir).unwrap();
    let profile = &config.agent_profiles.list[0];
    assert_eq!(profile.api_base_url, "https://legacy.example.com");
    assert_eq!(profile.api_key, "sk-legacy");
    assert_eq!(fs::read_to_string(&path).unwrap(), LEGACY_PROFILE_TOML);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn legacy_plaintext_credentials_migrate_on_save() {
    let config: Config = parse_toml(LEGACY_PROFILE_TOML).unwrap();
    let mut doc = LEGACY_PROFILE_TOML.parse::<DocumentMut>().unwrap();
    profile::patch_agent_document(&mut doc, &config.agent_profiles.list, "Legacy").unwrap();
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
    let stored = credentials::encrypt("https://current.example.com", "sk-current").unwrap();
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
    let valid = credentials::encrypt("https://real.example.com", "sk-real").unwrap();
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
    let base = PathBuf::from("NiumaTerm");
    assert_eq!(config_dir_for_mode(base.clone(), false), base);
    assert_eq!(config_dir_for_mode(base.clone(), true), base.join("Test"));
}

fn tmp_dir() -> PathBuf {
    env::temp_dir()
}

fn create_temporary_config(prefix: &str, toml_str: &str) -> Config {
    let file_name = tmp_dir().join(format!("test-rio-{prefix}-config.toml"));
    let mut file = fs::File::create(&file_name).unwrap();
    writeln!(file, "{toml_str}").unwrap();

    match Config::load_from_path_without_fallback(&file_name) {
        Ok(config) => config,
        Err(e) => panic!("{e}"),
    }
}

/// Terminal palette of the built-in default theme, which a config that
/// doesn't name a theme resolves to.
fn default_theme_colors() -> Colors {
    parse_toml::<Theme>(get_builtin_theme(&default_theme()).unwrap())
        .unwrap()
        .colors
        .terminal
}

fn create_temporary_theme(theme: &str, toml_str: &str) {
    let file_name = tmp_dir().join(theme).with_extension("toml");
    let mut file = fs::File::create(file_name).unwrap();
    writeln!(file, "{toml_str}").unwrap();
}

#[test]
fn test_filepath_does_not_exist_without_fallback() {
    let should_fail =
        Config::load_from_path_without_fallback(&tmp_dir().join("it-should-never-exist"));
    assert!(should_fail.is_err(), "{}", true);
}

#[test]
fn test_filepath_does_not_exist_with_fallback() {
    let config = Config::load_from_path(&tmp_dir().join("it-should-never-exist"));
    assert_eq!(config.theme, default_theme());
    assert_eq!(config.cursor.shape, default_cursor());
}

#[test]
fn startup_load_defaults_when_missing_and_errors_on_bad_toml() {
    let dir = tmp_dir().join("NiumaTerm-startup-config-test");
    let _ = fs::remove_dir_all(&dir);
    let path = dir.join("config.toml");

    let missing = Config::load_for_startup_from(&path, &dir).unwrap();
    assert_eq!(missing, Config::default());

    fs::create_dir_all(&dir).unwrap();
    fs::write(&path, "not [ valid").unwrap();
    assert!(Config::load_for_startup_from(&path, &dir).is_err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_if_explicit_defaults_match() {
    // An empty config file must resolve to the explicit defaults.
    let result = create_temporary_config("defaults", "");

    assert_eq!(result.cursor.shape, default_cursor());
    assert_eq!(result.theme, default_theme());
    assert_eq!(result.cursor.shape, default_cursor());
    assert_eq!(result.shell, default_shell());

    // Colors
    assert_eq!(result.colors, default_theme_colors());
}

#[test]
fn test_invalid_config_file() {
    let toml_str = r#"
            Performance = 2
            width = "big"
            height = "small"
        "#;

    let file_name = tmp_dir()
        .join("test-rio-invalid-config")
        .with_extension("toml");
    let mut file = fs::File::create(&file_name).unwrap();
    writeln!(file, "{toml_str}").unwrap();

    let result = Config::load_from_path(&file_name);

    assert_eq!(result.theme, default_theme());
    // Colors
    assert_eq!(result.colors.background, colors::defaults::background());
    assert_eq!(result.colors.foreground, colors::defaults::foreground());
    assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
    assert_eq!(result.colors.cursor, colors::defaults::cursor());
}

#[test]
fn test_change_config_cursor() {
    let result = create_temporary_config(
        "change-cursor",
        r#"
            [cursor]
            shape = 'underline'
        "#,
    );

    assert_eq!(result.cursor.shape, CursorShape::Underline);
    assert_eq!(result.theme, default_theme());
    // Colors
    assert_eq!(result.colors, default_theme_colors());

    let result = create_temporary_config(
        "change-cursor-line",
        r#"
            [cursor]
            shape = 'line'
        "#,
    );
    assert_eq!(result.cursor.shape, CursorShape::Beam);
}

#[test]
fn test_change_theme() {
    let result = create_temporary_config(
        "change-theme",
        r#"
            theme = "lucario"
        "#,
    );

    assert_eq!(result.theme, "lucario");
    // Colors
    assert_eq!(result.colors.background, colors::defaults::background());
    assert_eq!(result.colors.foreground, colors::defaults::foreground());
    assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
    assert_eq!(result.colors.cursor, colors::defaults::cursor());
}

#[test]
fn test_change_theme_with_colors() {
    create_temporary_theme(
        "lucario-with-colors",
        r#"
            name = 'Lucario'
            mode = 'dark'

            [colors.terminal]
            background       = '#2B3E50'
            foreground       = '#F8F8F2'

            [colors.ui]
            background = '#2B3E50'
        "#,
    );

    let result = create_temporary_config(
        "change-theme-with-colors",
        r#"
            theme = "lucario-with-colors"
        "#,
    );

    // Colors
    assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
    assert_eq!(result.colors.cursor, colors::defaults::cursor());
    assert_eq!(result.colors.foreground, hex_to_color_arr("#F8F8F2"));
    assert_eq!(result.colors.background.0, hex_to_color_arr("#2B3E50"));
    assert_eq!(result.ui_theme.as_ref().unwrap().name, "Lucario");
    assert_eq!(
        result.ui_theme.as_ref().unwrap().mode,
        AppearanceTheme::Dark
    );
    assert_eq!(
        result.ui_theme.as_ref().unwrap().colors["background"].as_str(),
        Some("#2B3E50")
    );
}

#[test]
fn theme_list_loads_valid_toml_files_in_name_order() {
    let dir = env::temp_dir().join("NiumaTerm-theme-list-test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
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

    let themes = Config::load_themes_from(&dir);
    assert_eq!(
        themes
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "Zulu"]
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn built_in_themes_load_without_user_files() {
    for builtin in BUILTIN_THEMES {
        let path = tmp_dir()
            .join("NiumaTerm-missing-builtins")
            .join(builtin.name)
            .with_extension("toml");
        let theme = Config::load_theme(&path).unwrap();
        assert!(!theme.name.is_empty());
    }
}

#[test]
fn custom_theme_overrides_builtin_case_insensitively() {
    let mut themes = vec![(String::from("ubuntu"), Theme::default())];
    merge_theme(&mut themes, (String::from("Ubuntu"), Theme::default()));

    assert_eq!(themes.len(), 1);
    assert_eq!(themes[0].0, "Ubuntu");
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

#[test]
fn test_shell() {
    let result = create_temporary_config(
        "change-shell-and-editor",
        r#"
            shell = { program = "/bin/fish", args = ["--hello"] }
        "#,
    );

    assert_eq!(result.shell.program, "/bin/fish");
    assert_eq!(result.shell.args, ["--hello"]);
}

#[test]
fn test_shell_no_args() {
    let result = create_temporary_config(
        "change-shell-and-editor-no-args",
        r#"
            shell = { program = "/bin/fish" }
        "#,
    );

    assert_eq!(result.shell.program, "/bin/fish");
    assert_eq!(result.shell.args, Vec::<&str>::new());
}

const EXAMPLE_CONFIG_PATH: &str = "../../assets/config-example.toml";

/// `assets/config-example.toml` documents every key with its built-in
/// default. Nothing regenerates it, so this compares it against the real
/// serialized default: a key added, removed, or renamed on `Config` fails
/// here instead of leaving the example advertising settings that no longer
/// exist. Run with `--nocapture` to print the replacement content.
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

#[test]
fn a_model_entry_carries_the_names_the_style_asks_for() {
    use agent::ModelListStyle;

    assert_eq!(
        ModelListStyle::NameAndId.label("Opus 5", "claude-opus-5"),
        "Opus 5 (claude-opus-5)"
    );
    assert_eq!(
        ModelListStyle::IdAndName.label("Opus 5", "claude-opus-5"),
        "claude-opus-5 (Opus 5)"
    );
    assert_eq!(
        ModelListStyle::NameOnly.label("Opus 5", "claude-opus-5"),
        "Opus 5"
    );
    assert_eq!(
        ModelListStyle::IdOnly.label("Opus 5", "claude-opus-5"),
        "claude-opus-5"
    );

    // A harness with one name for a model states it once under every style.
    assert_eq!(ModelListStyle::NameAndId.label("gpt-5", "gpt-5"), "gpt-5");
    assert_eq!(ModelListStyle::IdAndName.label("", "gpt-5"), "gpt-5");
    assert_eq!(ModelListStyle::NameOnly.label("  ", "gpt-5"), "gpt-5");
}
