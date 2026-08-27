use crate::claude_code::usage_fetcher::*;

#[test]
fn credentials_path_prefers_an_explicit_claude_config_dir() {
    assert_eq!(
        credentials_path(
            Some(OsStr::new(r"D:\profiles\claude")),
            Some(Path::new(r"C:\Users\test")),
        ),
        Some(PathBuf::from(r"D:\profiles\claude\.credentials.json"))
    );
    assert_eq!(
        credentials_path(None, Some(Path::new(r"C:\Users\test"))),
        Some(PathBuf::from(r"C:\Users\test\.claude\.credentials.json"))
    );
}

#[test]
fn reads_only_a_nonempty_claude_oauth_access_token() {
    assert_eq!(
        parse_oauth_token(
            br#"{"claudeAiOauth":{"accessToken":"  oauth-token  "},"apiKey":"ignored"}"#
        )
        .unwrap(),
        "oauth-token"
    );
    assert!(parse_oauth_token(br#"{"anthropicApiKey":"api-key"}"#).is_err());
    assert!(parse_oauth_token(br#"{"claudeAiOauth":{"accessToken":" "}}"#).is_err());
}

#[test]
fn the_panel_supplement_runs_only_for_a_live_reading_without_fable() {
    // Cancelled up front, so the guard is exercised without an
    // interactive Claude process: reaching the panel returns immediately.
    let cancelled = AtomicBool::new(true);
    let five_hour = Some(UsageWindow::new(88, FIVE_HOUR_WINDOW_MINUTES));
    let fable = Some(UsageWindow::new(35, WEEKLY_WINDOW_MINUTES));

    // A reading that already covers Fable has nothing to supplement.
    let complete = UsageSnapshot {
        five_hour: five_hour.clone(),
        fable_weekly: fable,
        ..UsageSnapshot::default()
    };
    assert_eq!(supplement_from_cli(complete.clone(), &cancelled), complete);

    // An endpoint that described no window at all describes an account the
    // panel cannot be trusted to describe either.
    let empty = UsageSnapshot::default();
    assert_eq!(supplement_from_cli(empty.clone(), &cancelled), empty);

    // A supplement that cannot run leaves the reading it was adding to.
    let partial = UsageSnapshot {
        five_hour,
        ..UsageSnapshot::default()
    };
    assert_eq!(supplement_from_cli(partial.clone(), &cancelled), partial);
}

#[test]
fn maps_oauth_windows_to_remaining_integer_percentages() {
    assert_eq!(
        parse_oauth_usage(
            br#"{"five_hour":{"utilization":12},"seven_day":{"used_percentage":34.4}}"#
        )
        .unwrap(),
        UsageSnapshot {
            five_hour: Some(UsageWindow::new(88, FIVE_HOUR_WINDOW_MINUTES)),
            weekly: Some(UsageWindow::new(66, WEEKLY_WINDOW_MINUTES)),
            ..UsageSnapshot::default()
        }
    );
    assert_eq!(
        parse_oauth_usage(br#"{"five_hour":{"utilization":120}}"#).unwrap(),
        UsageSnapshot {
            five_hour: Some(UsageWindow::new(0, FIVE_HOUR_WINDOW_MINUTES)),
            ..UsageSnapshot::default()
        }
    );
    assert_eq!(
        parse_oauth_usage(br#"{"fable_seven_day":{"utilization":12,"resets_at":1770000000}}"#)
            .unwrap(),
        UsageSnapshot {
            fable_weekly: Some(UsageWindow {
                remaining_percentage: 88,
                window_minutes: WEEKLY_WINDOW_MINUTES,
                resets_at: Some(1_770_000_000_000),
                reset_description: None,
            }),
            ..UsageSnapshot::default()
        }
    );
}

#[test]
fn oauth_fallback_is_limited_to_recoverable_statuses() {
    assert!(oauth_status_allows_cli_fallback(StatusCode::UNAUTHORIZED));
    assert!(oauth_status_allows_cli_fallback(
        StatusCode::INTERNAL_SERVER_ERROR
    ));
    assert!(!oauth_status_allows_cli_fallback(StatusCode::FORBIDDEN));
    assert!(!oauth_status_allows_cli_fallback(
        StatusCode::TOO_MANY_REQUESTS
    ));
    assert!(!oauth_status_allows_cli_fallback(StatusCode::NOT_FOUND));
}

#[test]
fn parses_interactive_usage_panel_with_split_labels_and_values() {
    let output = "\u{1b}[32mCurrent session\u{1b}[0m\r\n████ 97% used\r\nCurrent week (all models)\r\n17% consumed\r\nCurrent week (Fable)\r\n32% used\r\n";
    assert_eq!(
        parse_output(output).unwrap(),
        UsageSnapshot {
            five_hour: Some(UsageWindow::new(3, FIVE_HOUR_WINDOW_MINUTES)),
            weekly: Some(UsageWindow::new(83, WEEKLY_WINDOW_MINUTES)),
            fable_weekly: Some(UsageWindow::new(68, WEEKLY_WINDOW_MINUTES)),
            ..UsageSnapshot::default()
        }
    );
}

#[test]
fn accepts_remaining_wording_and_weekly_label_variants() {
    let output = "Current session: 62% left\nWeekly limits\n41.6% available\n";
    assert_eq!(
        parse_output(output).unwrap(),
        UsageSnapshot {
            five_hour: Some(UsageWindow::new(62, FIVE_HOUR_WINDOW_MINUTES)),
            weekly: Some(UsageWindow::new(42, WEEKLY_WINDOW_MINUTES)),
            ..UsageSnapshot::default()
        }
    );
}

#[test]
fn keeps_cli_reset_descriptions_with_their_windows() {
    let output = "Current session\n62% left\nResets in 2h 5m\nCurrent week (all models)\n42% left\nResets Tue 9:00 AM\n";
    let usage = parse_output(output).unwrap();

    assert_eq!(
        usage.five_hour.unwrap().reset_description.as_deref(),
        Some("Resets in 2h 5m")
    );
    assert_eq!(
        usage.weekly.unwrap().reset_description.as_deref(),
        Some("Resets Tue 9:00 AM")
    );
}

#[test]
fn keeps_cli_output_bounded_to_the_newest_bytes() {
    let mut output = b"old".to_vec();
    append_bounded(&mut output, b"-new-data", 8);
    assert_eq!(output, b"new-data");
}

/// An already-cancelled request must report cancellation rather than a
/// failure message: the caller restarts the first and only reports the
/// second, and it reaches neither the network nor an interactive CLI here.
#[test]
fn a_cancelled_request_reports_cancellation_not_failure() {
    let error = fetch_with_cancel(&AtomicBool::new(true))
        .expect_err("a cancelled fetch produces no snapshot");
    assert!(
        matches!(error, UsageFetchError::Cancelled),
        "expected Cancelled, got {error:?}"
    );
}
