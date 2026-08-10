use std::mem::take;

use serde_json::Value;

use crate::chat::{SkillCatalog, SkillInfo};

pub(super) fn skill_catalog_from_response(message: &Value) -> SkillCatalog {
    if let Some(error) = message["error"]["message"].as_str() {
        return SkillCatalog {
            skills: Vec::new(),
            errors: vec![format!("Codex skill catalog is unavailable: {error}")],
        };
    }

    parse_skill_catalog(&message["result"])
}

pub(super) fn parse_skill_catalog(result: &Value) -> SkillCatalog {
    let mut catalog = SkillCatalog::default();

    for entry in result["data"].as_array().into_iter().flatten() {
        for error in entry["errors"].as_array().into_iter().flatten() {
            let message = error["message"]
                .as_str()
                .unwrap_or("unknown skill loading error");
            let path = error["path"].as_str().unwrap_or_default();

            catalog.errors.push(if path.is_empty() {
                message.to_string()
            } else {
                format!("{message} ({path})")
            });
        }

        for skill in entry["skills"].as_array().into_iter().flatten() {
            let (Some(name), Some(description), Some(path), Some(scope), Some(enabled)) = (
                skill["name"].as_str(),
                skill["description"].as_str(),
                skill["path"].as_str(),
                skill["scope"].as_str(),
                skill["enabled"].as_bool(),
            ) else {
                continue;
            };

            if name.is_empty() || path.is_empty() {
                continue;
            }

            catalog.skills.push(SkillInfo {
                name: name.to_string(),
                description: description.to_string(),
                path: path.to_string(),
                scope: scope.to_string(),
                enabled,
                display_name: skill["interface"]["displayName"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
            });
        }
    }

    catalog
}

#[derive(Default)]
pub(super) struct SkillRefreshState {
    pub(super) in_flight: Option<u64>,
    force_reload_queued: bool,
}

impl SkillRefreshState {
    /// Return true when an active request owns refresh scheduling and the
    /// caller must not allocate another request id yet.
    pub(super) fn queue_if_in_flight(&mut self, force_reload: bool) -> bool {
        if self.in_flight.is_none() {
            return false;
        }

        self.force_reload_queued |= force_reload;
        true
    }

    pub(super) fn start(&mut self, rpc_id: u64) {
        debug_assert!(self.in_flight.is_none());
        self.in_flight = Some(rpc_id);
    }

    /// Complete only the current request and report whether invalidations
    /// accumulated while it was in flight.
    pub(super) fn finish(&mut self, rpc_id: u64) -> Option<bool> {
        if self.in_flight != Some(rpc_id) {
            return None;
        }

        self.in_flight = None;
        Some(take(&mut self.force_reload_queued))
    }
}
