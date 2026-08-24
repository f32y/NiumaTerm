use crate::launcher::*;

fn cmd_launcher() -> AgentCli {
    AgentCli::new("cmd.exe", [])
}

#[test]
fn bounded_runner_retains_suffix_and_redacts_environment_values() {
    let secret = "secret-value-for-test";
    let launcher = AgentCli::new(
        "cmd.exe",
        [("NMT_TEST_SECRET".to_string(), secret.to_string())],
    );
    let output = run_bounded(
        &launcher,
        ["/D", "/C", "echo 1234567890%NMT_TEST_SECRET%"],
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
    let launcher = AgentCli::new(
        "cmd.exe",
        [("NMT_TEST_VALUE".to_string(), "codex".to_string())],
    );
    let output = run_bounded(
        &launcher,
        ["/D", "/C", "echo {\"codexVersion\":\"1.2.3\"}"],
        ProcessLimits::new(Duration::from_secs(3), 256),
    )
    .unwrap();

    assert!(output.stdout.contains("<redacted>Version"));
    assert!(output.stdout_for_parsing().contains("codexVersion"));
    assert!(!format!("{output:?}").contains("codexVersion"));
}

#[test]
fn bounded_runner_times_out_and_reports_bounded_diagnostics() {
    let error = run_bounded(
        &cmd_launcher(),
        ["/D", "/C", "echo before-timeout & ping -n 6 127.0.0.1 >nul"],
        ProcessLimits::new(Duration::from_millis(100), 64),
    )
    .unwrap_err();
    assert!(matches!(error, ProcessError::TimedOut { .. }));
    assert!(error.to_string().len() < 4_200);
}
