use nmt_config::system::*;

#[test]
fn system_section_defaults_when_absent() {
    let config: nmt_config::Config = toml::from_str("").unwrap();
    assert_eq!(config.system, SystemConfig::default());
    assert!(config.system.restore_last_session_when_opening);
    assert!(!config.system.manage_subprocess_job);
    assert_eq!(
        config.system.warn_before_terminating_shell,
        WarnBeforeTerminatingShell::WhenChildProcessesRunning
    );
    assert!(config.system.confirm_before_closing_workspace);
    assert!(!config.system.prioritize_ui_threads);
}

#[test]
fn warn_before_terminating_shell_accepts_modes_and_rejects_booleans() {
    for (value, expected) in [
        ("\"disabled\"", WarnBeforeTerminatingShell::Disabled),
        (
            "\"when-child-processes-running\"",
            WarnBeforeTerminatingShell::WhenChildProcessesRunning,
        ),
        ("\"always\"", WarnBeforeTerminatingShell::Always),
    ] {
        let config: nmt_config::Config = toml::from_str(&format!(
            "[system]\nwarn-before-terminating-shell = {value}"
        ))
        .unwrap();
        assert_eq!(config.system.warn_before_terminating_shell, expected);
    }

    assert!(
        toml::from_str::<nmt_config::Config>("[system]\nwarn-before-terminating-shell = true")
            .is_err()
    );
}

#[test]
fn warn_mode_decides_from_child_process_count() {
    assert!(!WarnBeforeTerminatingShell::Disabled.should_warn(1));
    assert!(!WarnBeforeTerminatingShell::WhenChildProcessesRunning.should_warn(0));
    assert!(WarnBeforeTerminatingShell::WhenChildProcessesRunning.should_warn(1));
    assert!(WarnBeforeTerminatingShell::Always.should_warn(0));
}
