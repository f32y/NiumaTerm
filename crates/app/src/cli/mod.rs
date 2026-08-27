//! `nmt://` command-line URL parsing.
//!
//! `nmt://action/new_tab?path=<p>` / `nmt://action/new_window?path=<p>` open a
//! directory; `nmt://action/activate` (internal, sent by an argument-less
//! second launch) only foregrounds the running instance.

use std::path::{self, Path, PathBuf};

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

            let path = path::absolute(Path::new(decoded.as_ref()))
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
mod tests;
