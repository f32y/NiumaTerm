//! Declaring a model as image-capable in the harness's own configuration.
//!
//! The harness admits an image only when the selected model reports `image`
//! among its input modalities, and a model its provider never advertised
//! resolves as text-only. A model a profile names by hand is therefore closed
//! to image input until the provider's configured catalog says otherwise,
//! which this writes through the same settings surface the harness's own
//! configuration form uses.

use serde_json::{Value, json};

use crate::deepseek::api::ApiClient;

/// How each adapter spells a catalog entry's input modalities. The field name
/// is the whole difference between them, and an adapter missing from this list
/// is left alone rather than written with entries its own schema would drop.
const MODALITY_FIELDS: [(&str, &str); 2] =
    [("llm-deepseek", "inputModalities"), ("llm-pi-ai", "input")];

/// Declare `model` image-capable on `provider`'s configured catalog, and
/// report whether that changed anything.
///
/// `Ok(false)` means the catalog already offered the model to images, which is
/// the ordinary state once this has run for a profile.
pub(super) fn declare_image_input(
    client: &ApiClient,
    provider: &str,
    model: &str,
) -> Result<bool, String> {
    let providers = client
        .call("llm.providers", json!({}))
        .map_err(|error| error.message().to_string())?;
    let route = providers["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|route| route["provider"].as_str() == Some(provider))
        .ok_or_else(|| format!("the harness does not configure the {provider} route"))?;

    let namespace = route["settingsNs"].as_str().unwrap_or_default();
    let Some((_, field)) = MODALITY_FIELDS
        .iter()
        .find(|(known, _)| *known == namespace)
    else {
        return Err(format!(
            "{provider} is configured by {namespace}, whose model catalog has a shape this cannot write"
        ));
    };
    // Where the provider's own settings live inside its section. Empty for a
    // section that is the provider profile itself, which is what the DeepSeek
    // route declares; the OpenAI-compatible adapter keeps one profile per
    // route instead.
    let section: Vec<&str> = route["settingsPath"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();

    let described = client
        .call("settings.describe", json!({}))
        .map_err(|error| error.message().to_string())?;
    if described["writable"] != Value::Bool(true) {
        return Err("the harness runs on settings it cannot write".to_string());
    }
    let view = described["namespaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|view| view["ns"].as_str() == Some(namespace))
        .ok_or_else(|| format!("the harness reports no {namespace} settings"))?;

    let Some(models) = models_with_image(catalog(view, &section), model, field) else {
        return Ok(false);
    };

    let mut path: Vec<&str> = section.clone();
    path.push("models");
    let mut payload = json!({
        "ns": namespace,
        "ops": [{ "op": "set", "path": path, "value": models }],
    });
    // The revision the catalog was read at, so a concurrent edit from the
    // harness's own settings form is refused instead of being overwritten.
    if let Some(revision) = view["revision"].as_u64() {
        payload["expectedRevision"] = json!(revision);
    }

    let written = client
        .call("settings.mutate", payload)
        .map_err(|error| error.message().to_string())?;

    // The answer is the namespace as the harness now reads it, which is the
    // only place a silently dropped declaration shows. An adapter build whose
    // catalog schema has no modality field accepts the write, stores it, and
    // resolves the entry without it — so the write has to be read back rather
    // than assumed, or every conversation would rewrite a setting that never
    // takes and the first image would still be refused with no explanation.
    if !declares_image(catalog(&written, &section), model, field) {
        return Err(format!(
            "the {namespace} adapter in this harness kept {model} without image input, so this build serves it as text only"
        ));
    }
    Ok(true)
}

/// The model catalog inside one namespace view.
fn catalog<'a>(view: &'a Value, section: &[&str]) -> &'a Value {
    let mut profile = &view["value"];
    for step in section {
        profile = &profile[*step];
    }
    &profile["models"]
}

/// Whether `models` offers `model` to image input.
fn declares_image(models: &Value, model: &str, field: &str) -> bool {
    models
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["id"].as_str() == Some(model))
        .and_then(|entry| entry[field].as_array())
        .is_some_and(|declared| declared.iter().any(|modality| modality == "image"))
}

/// The catalog that declares `model` image-capable, or `None` when the one
/// given already does.
///
/// The whole array is rewritten because a settings write replaces the value at
/// its path: an entry appended on its own would drop every model the array
/// already held. That does pin the adapter's built-in catalog into the user's
/// own settings, which is the same thing editing the array in the harness's
/// configuration form does.
pub(super) fn models_with_image(models: &Value, model: &str, field: &str) -> Option<Value> {
    if declares_image(models, model, field) {
        return None;
    }

    let mut catalog: Vec<Value> = models.as_array().cloned().unwrap_or_default();
    let modalities = json!(["text", "image"]);

    match catalog
        .iter_mut()
        .find(|entry| entry["id"].as_str() == Some(model))
    {
        Some(entry) => entry[field] = modalities,
        // A model the catalog never listed carries nothing else: the adapter
        // fills a context window and an image budget of its own for an entry
        // that names only its modalities.
        None => catalog.push(json!({ "id": model, field: modalities })),
    }

    Some(Value::Array(catalog))
}
