use std::env;

use crate::cli::*;

#[test]
fn parses_new_tab() {
    let action = parse_nmt_url("nmt://action/new_tab?path=C%3A%2FA%2FB").unwrap();
    assert_eq!(
        action,
        CliAction::NewTab {
            path: PathBuf::from("C:\\A\\B")
        }
    );
}

#[test]
fn parses_new_window() {
    let action = parse_nmt_url("nmt://action/new_window?path=C:/A").unwrap();
    assert_eq!(
        action,
        CliAction::NewWindow {
            path: PathBuf::from("C:\\A")
        }
    );
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
    let action =
        parse_nmt_url("nmt://action/new_tab?path=C%3A%2FMy%20Dir%2F%E9%A1%B9%E7%9B%AE").unwrap();
    assert_eq!(
        action,
        CliAction::NewTab {
            path: PathBuf::from("C:\\My Dir\\项目")
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
    assert_eq!(path, env::current_dir().unwrap().join("sub\\dir"));
}

#[test]
fn url_round_trips_through_to_url() {
    let action = CliAction::NewWindow {
        path: PathBuf::from("C:\\My Dir\\项目"),
    };
    assert_eq!(parse_nmt_url(&action.to_url()).unwrap(), action);
    assert_eq!(
        parse_nmt_url(&CliAction::Activate.to_url()).unwrap(),
        CliAction::Activate
    );
}

#[test]
fn focus_notification_round_trips_and_rejects_invalid_ids() {
    let action = CliAction::FocusNotification {
        route: AgentRoute::parse("process:pane").unwrap(),
        notification_id: "process:pane:1".into(),
    };
    assert_eq!(parse_nmt_url(&action.to_url()).unwrap(), action);
    assert!(parse_nmt_url("nmt://action/focus_notification?route=a").is_err());
    assert!(
        parse_nmt_url(&format!(
            "nmt://action/focus_notification?route=a&notification_id={}",
            "x".repeat(513)
        ))
        .is_err()
    );
}
