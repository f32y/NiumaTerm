//! Small `serde_json::Value` readers shared by the provider adapters, which
//! all fold loosely-shaped provider records into the same presentation
//! strings.

use serde_json::Value;

/// The first of `keys` whose value is a non-empty string, trimmed. Provider
/// records name the same field differently across versions, so readers probe
/// the known spellings in order.
pub(crate) fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value[*key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

/// One-line preview of free-form text: whitespace runs collapse to single
/// spaces and long text is cut with an ellipsis, because the rows these
/// previews land in are one line tall. `None` for all-whitespace text, so a
/// blank record falls through to the caller's next candidate.
pub(crate) fn condense(text: &str) -> Option<String> {
    const MAX_PREVIEW_CHARS: usize = 160;
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    Some(match text.char_indices().nth(MAX_PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    })
}
