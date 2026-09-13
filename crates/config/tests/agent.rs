#![cfg(feature = "application")]

use nmt_config::Config;
use nmt_config::agent::AgentConfig;
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
