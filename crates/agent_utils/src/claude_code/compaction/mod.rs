//! Compaction-boundary metadata shared by the live stream-json protocol and
//! persisted session replay.
//!
//! The two carry the same record under different key conventions: the SDK
//! output message uses `compact_metadata` with snake_case fields, while the
//! transcript file keeps the CLI's internal `compactMetadata` with camelCase
//! fields. Reading both spellings from one parser keeps a resumed boundary as
//! detailed as a live one.

use serde_json::Value;

use crate::chat::{Compaction, CompactionTrigger};

/// The metadata object of a `compact_boundary` record, whichever key
/// convention produced it.
pub(super) fn compaction_metadata(record: &Value) -> &Value {
    let snake = &record["compact_metadata"];

    if snake.is_object() {
        snake
    } else {
        &record["compactMetadata"]
    }
}

pub(super) fn parse_compaction(metadata: &Value) -> Compaction {
    Compaction {
        trigger: match metadata["trigger"].as_str() {
            Some("auto") => Some(CompactionTrigger::Automatic),
            Some("manual") => Some(CompactionTrigger::Manual),
            _ => None,
        },
        pre_tokens: token_count(metadata, "pre_tokens", "preTokens"),
        post_tokens: token_count(metadata, "post_tokens", "postTokens"),
        messages_summarized: token_count(metadata, "messages_summarized", "messagesSummarized"),
        user_context: text(metadata, "user_context", "userContext"),
        summary: None,
    }
}

fn token_count(metadata: &Value, snake: &str, camel: &str) -> Option<u64> {
    metadata[snake]
        .as_u64()
        .or_else(|| metadata[camel].as_u64())
}

fn text(metadata: &Value, snake: &str, camel: &str) -> Option<String> {
    metadata[snake]
        .as_str()
        .or_else(|| metadata[camel].as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests;
