use nmt_config::Config;
use nmt_config::agent::AgentConfig;
use toml::from_str;

#[test]
fn agent_section_defaults_when_absent() {
    let config: Config = from_str("").unwrap();
    assert_eq!(config.agent, AgentConfig::default());
    assert!(config.agent.enable_agent_hooks);
    assert!(config.agent.show_agent_usage);
}
