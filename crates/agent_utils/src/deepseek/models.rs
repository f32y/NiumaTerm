//! The session's model directory.
//!
//! The harness addresses a model by the pair `(provider route, model id)`,
//! while the pane's picker submits one opaque string. This holds the mapping
//! between the two, which is also why the directory outlives the frame that
//! carried it: a later pick has to be turned back into the pair.

use serde_json::Value;

use crate::chat::ModelInfo;

/// One selectable model, in both vocabularies.
struct ModelRoute {
    /// What the picker submits and displays as its value.
    key: String,
    provider: String,
    model: String,
    display: String,
    /// Reasoning-effort ids this exact model route advertises. Empty when the
    /// adapter exposes no effort control for it.
    efforts: Vec<String>,
}

/// Every model this session can reach, plus what it is currently set to.
#[derive(Default)]
pub(crate) struct ModelDirectory {
    routes: Vec<ModelRoute>,
    /// Route the session is currently on. An id the catalog never listed is
    /// addressed to this provider, because a bare id names no route of its own.
    current_provider: String,
    /// Key of the current selection, absent only before the first catalog.
    selected: Option<String>,
    effort: Option<String>,
}

impl ModelDirectory {
    /// Read a `session.models` result.
    ///
    /// Catalog membership is advisory: a route can serve a model it stopped
    /// advertising. So the current selection is added as its own entry when the
    /// groups do not list it, because the alternative is a picker that shows no
    /// value while the session runs perfectly well.
    pub(crate) fn parse(value: &Value) -> Self {
        let groups = value["groups"].as_array().cloned().unwrap_or_default();

        // A model id served by two providers cannot address either one on its
        // own, so only the ambiguous ids grow the provider prefix. Keeping the
        // bare id everywhere else is what lets a profile name a model the way
        // its provider does.
        let mut routes: Vec<ModelRoute> = Vec::new();
        for group in &groups {
            let provider = group["id"].as_str().unwrap_or_default();
            let provider_name = group["name"].as_str().unwrap_or(provider);
            for model in group["models"].as_array().into_iter().flatten() {
                let Some(id) = model["id"].as_str() else {
                    continue;
                };
                let ambiguous = groups
                    .iter()
                    .filter(|other| contains_model(other, id))
                    .count()
                    > 1;
                let name = model["name"].as_str().unwrap_or(id);

                routes.push(ModelRoute {
                    key: if ambiguous {
                        format!("{provider}/{id}")
                    } else {
                        id.to_string()
                    },
                    provider: provider.to_string(),
                    model: id.to_string(),
                    display: if ambiguous {
                        format!("{name} ({provider_name})")
                    } else {
                        name.to_string()
                    },
                    efforts: model["reasoning"]["efforts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|effort| Some(effort["id"].as_str()?.to_string()))
                        .collect(),
                });
            }
        }

        let current = &value["current"];
        let provider = current["provider"].as_str().unwrap_or_default();
        let model = current["model"].as_str().unwrap_or_default();
        let selected = match routes
            .iter()
            .find(|route| route.provider == provider && route.model == model)
        {
            Some(route) => Some(route.key.clone()),
            None if !model.is_empty() => {
                routes.push(ModelRoute {
                    key: model.to_string(),
                    provider: provider.to_string(),
                    model: model.to_string(),
                    display: model.to_string(),
                    efforts: Vec::new(),
                });
                Some(model.to_string())
            }
            None => None,
        };

        Self {
            routes,
            current_provider: provider.to_string(),
            selected,
            effort: current["reasoningEffort"].as_str().map(str::to_string),
        }
    }

    pub(crate) fn catalog(&self) -> Vec<ModelInfo> {
        self.routes
            .iter()
            .map(|route| ModelInfo {
                model: route.key.clone(),
                display: route.display.clone(),
                // Service tiers are a Codex concept; the harness has none.
                tiers: Vec::new(),
                default_tier: None,
                efforts: route.efforts.clone(),
            })
            .collect()
    }

    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub(crate) fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    /// The `(provider, model)` pair a picked key addresses.
    ///
    /// Catalog membership is advisory on the way in as well as the way out: a
    /// provider resolves an id it never advertised as a text-only model on its
    /// own route, so an id absent from the directory is still addressable and
    /// gets routed rather than refused.
    ///
    /// A prefix is read as a provider only when some route actually serves that
    /// provider, because a model id may contain a slash of its own
    /// (`Qwen/Qwen3-32B`) and splitting one would address a provider that does
    /// not exist.
    pub(crate) fn route<'a>(&'a self, key: &'a str) -> (&'a str, &'a str) {
        if let Some(route) = self.routes.iter().find(|route| route.key == key) {
            return (route.provider.as_str(), route.model.as_str());
        }

        match key.split_once('/') {
            Some((provider, model))
                if self.routes.iter().any(|route| route.provider == provider) =>
            {
                (provider, model)
            }
            _ => (self.current_provider.as_str(), key),
        }
    }

    /// Record a selection the harness confirmed.
    pub(crate) fn set_selected(&mut self, key: String, effort: Option<String>) {
        self.selected = Some(key);
        self.effort = effort;
    }
}

fn contains_model(group: &Value, id: &str) -> bool {
    group["models"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|model| model["id"].as_str() == Some(id))
}
