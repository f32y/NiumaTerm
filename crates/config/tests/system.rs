use nmt_config::system::*;

#[test]
fn system_section_defaults_when_absent() {
    let config: nmt_config::Config = toml::from_str("").unwrap();
    assert_eq!(config.system, SystemConfig::default());
    assert!(config.system.restore_last_session_when_opening);
    assert!(!config.system.manage_subprocess_job);
    assert!(config.system.warn_before_terminating_shell);
    assert!(config.system.confirm_before_closing_workspace);
    assert!(!config.system.prioritize_ui_threads);
}
