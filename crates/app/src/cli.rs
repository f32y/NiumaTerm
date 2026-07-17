//! `nmt://` command-line URL parsing.
//!
//! `nmt://action/new_tab?path=<p>` / `nmt://action/new_window?path=<p>` open a
//! directory; `nmt://action/activate` (internal, sent by an argument-less
//! second launch) only foregrounds the running instance.

use std::path::{Path, PathBuf};

use nmt_agent_utils::AgentRoute;
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CliAction {
    NewTab {
        path: PathBuf,
    },
    NewWindow {
        path: PathBuf,
    },
    Activate,
    FocusNotification {
        route: AgentRoute,
        notification_id: String,
    },
}

impl CliAction {
    /// The action as an `nmt://` URL, for forwarding over the IPC pipe. The
    /// path is absolute here (parsing resolved it against the caller's cwd),
    /// so the primary decodes the same directory regardless of its own cwd.
    pub(crate) fn to_url(&self) -> String {
        match self {
            Self::Activate => "nmt://action/activate".to_string(),
            Self::NewTab { path } => format!("nmt://action/new_tab?path={}", encode_path(path)),
            Self::NewWindow { path } => {
                format!("nmt://action/new_window?path={}", encode_path(path))
            }
            Self::FocusNotification {
                route,
                notification_id,
            } => format!(
                "nmt://action/focus_notification?route={}&notification_id={}",
                utf8_percent_encode(route.as_str(), NON_ALPHANUMERIC),
                utf8_percent_encode(notification_id, NON_ALPHANUMERIC)
            ),
        }
    }
}

fn encode_path(path: &Path) -> String {
    utf8_percent_encode(&path.display().to_string(), NON_ALPHANUMERIC).to_string()
}

pub(crate) fn path_action_url(action: &str, path: &str) -> String {
    format!(
        "nmt://action/{action}?path={}",
        utf8_percent_encode(path, NON_ALPHANUMERIC)
    )
}

/// Parse an `nmt://` URL into an action. Relative paths resolve against the
/// current process's working directory (callers forward the re-encoded
/// absolute form). Errors describe why the URL was rejected; the caller logs
/// and ignores.
pub(crate) fn parse_nmt_url(url: &str) -> Result<CliAction, String> {
    let rest = url
        .strip_prefix("nmt://action/")
        .ok_or_else(|| format!("not an nmt://action/ url: {url}"))?;
    let (verb, query) = rest.split_once('?').unwrap_or((rest, ""));
    match verb {
        "activate" => Ok(CliAction::Activate),
        "focus_notification" => {
            let value = |name: &str| {
                query
                    .split('&')
                    .find_map(|part| part.strip_prefix(&format!("{name}=")))
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| format!("missing {name} in {url}"))
                    .and_then(|value| {
                        percent_decode_str(value)
                            .decode_utf8()
                            .map(|value| value.into_owned())
                            .map_err(|_| format!("{name} in {url} is not UTF-8"))
                    })
            };
            let route = AgentRoute::parse(&value("route")?)
                .map_err(|_| format!("invalid route in {url}"))?;
            let notification_id = value("notification_id")?;
            if notification_id.len() > 512 || notification_id.chars().any(char::is_control) {
                return Err(format!("invalid notification_id in {url}"));
            }
            Ok(CliAction::FocusNotification {
                route,
                notification_id,
            })
        }
        "new_tab" | "new_window" => {
            let encoded = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("path="))
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("missing path in {url}"))?;
            let decoded = percent_decode_str(encoded)
                .decode_utf8()
                .map_err(|err| format!("path in {url} is not UTF-8: {err}"))?;
            let path = std::path::absolute(Path::new(decoded.as_ref()))
                .map_err(|err| format!("cannot resolve path in {url}: {err}"))?;
            Ok(if verb == "new_tab" {
                CliAction::NewTab { path }
            } else {
                CliAction::NewWindow { path }
            })
        }
        other => Err(format!("unknown action {other:?} in {url}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            parse_nmt_url("nmt://action/new_tab?path=C%3A%2FMy%20Dir%2F%E9%A1%B9%E7%9B%AE")
                .unwrap();
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
        assert_eq!(path, std::env::current_dir().unwrap().join("sub\\dir"));
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
}
