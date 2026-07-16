use nmt_config::agent::AgentConfig;

#[test]
fn agent_section_defaults_when_absent() {
    let config: nmt_config::Config = toml::from_str("").unwrap();
    assert_eq!(config.agent, AgentConfig::default());
    assert!(config.agent.enable_agent_hooks);
    assert!(config.agent.show_agent_usage);
}
