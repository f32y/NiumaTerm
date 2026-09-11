use std::env;

use crate::cli::*;

/// An absolute path spelled the way this platform spells one, and the same
/// path percent-encoded. A Windows literal is a *relative* path on a Unix
/// host, so the parser would resolve it against the working directory rather
/// than round-trip it, and the assertion would be about the wrong thing.
#[cfg(windows)]
fn absolute(segments: &[&str]) -> PathBuf {
    format!(r"C:\{}", segments.join(r"\")).into()
}

#[cfg(unix)]
fn absolute(segments: &[&str]) -> PathBuf {
    format!("/{}", segments.join("/")).into()
}

#[cfg(windows)]
const ENCODED_A_B: &str = "C%3A%2FA%2FB";
#[cfg(unix)]
const ENCODED_A_B: &str = "%2FA%2FB";

#[test]
fn parses_new_tab() {
    let action = parse_nmt_url(&format!("nmt://action/new_tab?path={ENCODED_A_B}")).unwrap();

    assert_eq!(
        action,
        CliAction::NewTab {
            path: absolute(&["A", "B"])
        }
    );
}

#[test]
fn parses_new_window() {
    let path = absolute(&["A"]);
    let action =
        parse_nmt_url(&format!("nmt://action/new_window?path={}", path.display())).unwrap();

    assert_eq!(action, CliAction::NewWindow { path });
}

#[test]
fn parses_activate() {
    assert_eq!(
        parse_nmt_url("nmt://action/activate").unwrap(),
        CliAction::Activate
    );
}

#[test]
fn decodes_spaces_and_cjk() {
    #[cfg(windows)]
    let encoded = "C%3A%2FMy%20Dir%2F%E9%A1%B9%E7%9B%AE";

    #[cfg(unix)]
    let encoded = "%2FMy%20Dir%2F%E9%A1%B9%E7%9B%AE";

    let action = parse_nmt_url(&format!("nmt://action/new_tab?path={encoded}")).unwrap();

    assert_eq!(
        action,
        CliAction::NewTab {
            path: absolute(&["My Dir", "项目"])
        }
    );
}

#[test]
fn rejects_unknown_verb_and_scheme() {
    assert!(parse_nmt_url("nmt://action/open?path=C:/A").is_err());
    assert!(parse_nmt_url("http://example.com").is_err());
}

#[test]
fn rejects_missing_or_empty_path() {
    assert!(parse_nmt_url("nmt://action/new_tab").is_err());
    assert!(parse_nmt_url("nmt://action/new_tab?path=").is_err());
    assert!(parse_nmt_url("nmt://action/new_tab?other=1").is_err());
}

#[test]
fn resolves_relative_path_against_cwd() {
    let action = parse_nmt_url("nmt://action/new_tab?path=sub%2Fdir").unwrap();
    let CliAction::NewTab { path } = action else {
        panic!("expected NewTab");
    };

    assert_eq!(path, env::current_dir().unwrap().join("sub").join("dir"));
}

#[test]
fn action_url_round_trip() {
    let action = CliAction::NewWindow {
        path: absolute(&["My Dir", "项目"]),
    };

    let url: String = (&action).into();
    assert_eq!(parse_nmt_url(&url).unwrap(), action);
    let activate_url: String = (&CliAction::Activate).into();
    assert_eq!(parse_nmt_url(&activate_url).unwrap(), CliAction::Activate);
}

#[test]
fn focus_notification_round_trips_and_rejects_invalid_ids() {
    let action = CliAction::FocusNotification {
        route: AgentRoute::parse("process:pane").unwrap(),
        notification_id: "process:pane:1".into(),
    };

    let url: String = (&action).into();
    assert_eq!(parse_nmt_url(&url).unwrap(), action);
    assert!(parse_nmt_url("nmt://action/focus_notification?route=a").is_err());
    assert!(
        parse_nmt_url(&format!(
            "nmt://action/focus_notification?route=a&notification_id={}",
            "x".repeat(513)
        ))
        .is_err()
    );
}
