//! Small `serde_json::Value` readers shared by the provider adapters, which
//! all fold loosely-shaped provider records into the same presentation
//! strings.

use chrono::{DateTime, SecondsFormat};
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

/// Readable text of a message or tool-result `content`, which providers
/// write either as a plain string or as an array of typed blocks. Text
/// blocks are joined with newlines. `text_blocks_only` skips blocks of other
/// types (`tool_use`, `image`) that happen to carry a `text` field. `None`
/// covers an unknown shape and content whose text is blank, so a caller can
/// fall through to its next candidate.
pub(crate) fn block_text(content: &Value, text_blocks_only: bool) -> Option<String> {
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| !text_blocks_only || block["type"].as_str() == Some("text"))
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };

    (!text.trim().is_empty()).then_some(text)
}

/// Unix seconds as the RFC 3339 form the session picker renders (UTC, `Z`
/// suffix, second precision), which is what backends reading their history
/// off disk already record. `None` for a value outside chrono's range, so a
/// corrupt stamp shows no date rather than the epoch.
pub(crate) fn rfc3339_from_unix_seconds(seconds: i64) -> Option<String> {
    DateTime::from_timestamp(seconds, 0).map(|date| date.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// An RFC 3339 stamp as Unix seconds, or `None` when it does not parse.
pub(crate) fn unix_seconds_from_rfc3339(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|date| date.timestamp())
}

/// A `-`/`+` line body from whole before and after texts. Providers report
/// file edits as complete old and new strings rather than hunks, and the
/// file-change card renders exactly this body.
pub(crate) fn diff_lines(removed: &str, added: &str) -> String {
    let mut diff = String::new();

    for line in removed.lines() {
        diff.push('-');

        diff.push_str(line);

        diff.push('\n');
    }

    for line in added.lines() {
        diff.push('+');

        diff.push_str(line);

        diff.push('\n');
    }

    diff
}
