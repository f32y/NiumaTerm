use crate::launcher::*;

#[test]
fn effective_environment_matches_child_case_rules_and_last_override() {
    let launcher = AgentCli::new(
        SHELL,
        [
            ("NMT_CASE_PROBE".into(), "first".into()),
            ("NMT_CASE_PROBE".into(), "upper".into()),
            ("nmt_case_probe".into(), "lower".into()),
        ],
    );
    #[cfg(windows)]
    let expected_upper = "lower";
    #[cfg(unix)]
    let expected_upper = "upper";
    assert_eq!(
        launcher.effective_env_os("NMT_CASE_PROBE"),
        Some(OsString::from(expected_upper))
    );
    assert_eq!(
        launcher.effective_env_os("nmt_case_probe"),
        Some(OsString::from("lower"))
    );
    #[cfg(windows)]
    let body = "if not %NMT_CASE_PROBE%==lower exit /b 9";
    #[cfg(unix)]
    let body = "[ \"$NMT_CASE_PROBE\" = upper ] && [ \"$nmt_case_probe\" = lower ]";
    let output = run_bounded(
        &launcher,
        script(body),
        ProcessLimits::new(Duration::from_secs(3), 1024),
    )
    .unwrap();
    assert!(output.success());
}

/// The bounded runner is exercised through a real child process, so each
/// assertion needs a shell that exists on the host. Only the spelling of the
/// script differs; every case tests the same runner behaviour.
#[cfg(windows)]
const SHELL: &str = "cmd.exe";
#[cfg(unix)]
const SHELL: &str = "/bin/sh";

#[cfg(windows)]
const SHELL_FLAGS: [&str; 2] = ["/D", "/C"];
#[cfg(unix)]
const SHELL_FLAGS: [&str; 1] = ["-c"];

fn script(body: &str) -> Vec<String> {
    SHELL_FLAGS
        .iter()
        .copied()
        .chain([body])
        .map(str::to_owned)
        .collect()
}

fn shell_launcher() -> AgentCli {
    AgentCli::new(SHELL, [])
}

#[test]
fn bounded_runner_retains_suffix_and_redacts_environment_values() {
    let secret = "secret-value-for-test";
    let launcher = AgentCli::new(SHELL, [("NMT_TEST_SECRET".to_string(), secret.to_string())]);
    #[cfg(windows)]
    let body = "echo 1234567890%NMT_TEST_SECRET%";
    #[cfg(unix)]
    let body = "echo \"1234567890$NMT_TEST_SECRET\"";
    let output = run_bounded(
        &launcher,
        script(body),
        ProcessLimits::new(Duration::from_secs(3), 20),
    )
    .unwrap();
    assert!(output.success());
    assert!(output.stdout_truncated);
    assert!(!output.stdout.contains(secret));
    assert!(output.stdout.contains("<redacted>"));
}

#[test]
fn structured_probe_parsing_precedes_diagnostic_redaction() {
    let launcher = AgentCli::new(SHELL, [("NMT_TEST_VALUE".to_string(), "codex".to_string())]);
    #[cfg(windows)]
    let body = "echo {\"codexVersion\":\"1.2.3\"}";
    #[cfg(unix)]
    let body = "echo '{\"codexVersion\":\"1.2.3\"}'";
    let output = run_bounded(
        &launcher,
        script(body),
        ProcessLimits::new(Duration::from_secs(3), 256),
    )
    .unwrap();

    assert!(output.stdout.contains("<redacted>Version"));
    assert!(output.stdout_for_parsing().contains("codexVersion"));
    assert!(!format!("{output:?}").contains("codexVersion"));
}

#[test]
fn bounded_runner_times_out_and_reports_bounded_diagnostics() {
    #[cfg(windows)]
    let body = "echo before-timeout & ping -n 6 127.0.0.1 >nul";
    #[cfg(unix)]
    let body = "echo before-timeout; sleep 6";
    let error = run_bounded(
        &shell_launcher(),
        script(body),
        ProcessLimits::new(Duration::from_millis(100), 64),
    )
    .unwrap_err();
    assert!(matches!(error, ProcessError::TimedOut { .. }));
    assert!(error.to_string().len() < 4_200);
}
