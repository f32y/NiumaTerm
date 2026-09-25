use nmt_config::Config;
use nmt_config::agent::{AgentConfig, TokenSpeedMode};
use toml::from_str;

#[test]
fn agent_section_defaults_when_absent() {
    let config: Config = from_str("").unwrap();

    assert_eq!(config.agent, AgentConfig::default());
    assert!(config.agent.enable_agent_hooks);
    assert!(config.agent.show_agent_usage);
    assert!(!config.agent.enable_agent_team);
}

#[test]
fn agent_team_requires_explicit_opt_in_in_existing_configs() {
    let config: Config = from_str("[agent]\nshow-agent-usage = false\n").unwrap();

    assert!(!config.agent.enable_agent_team);

    for enabled in [true, false] {
        let config: Config =
            from_str(&format!("[agent]\nenable-agent-team = {enabled}\n")).unwrap();

        assert_eq!(config.agent.enable_agent_team, enabled);

        let saved = toml::to_string(&config).unwrap();
        let restored: Config = from_str(&saved).unwrap();

        assert_eq!(restored.agent.enable_agent_team, enabled);
    }
}

#[test]
fn token_speed_mode_defaults_to_session_and_round_trips_both_choices() {
    for text in ["", "[agent]\nshow-agent-usage = false\n"] {
        let config: Config = from_str(text).unwrap();

        assert_eq!(config.agent.token_speed_mode, TokenSpeedMode::Session);
    }

    for (key, mode) in [
        ("session", TokenSpeedMode::Session),
        ("current-turn", TokenSpeedMode::CurrentTurn),
    ] {
        let config: Config = from_str(&format!("[agent]\ntoken-speed-mode = \"{key}\"\n")).unwrap();

        assert_eq!(config.agent.token_speed_mode, mode);

        let restored: Config = from_str(&toml::to_string(&config).unwrap()).unwrap();

        assert_eq!(restored.agent.token_speed_mode, mode);
    }
}
