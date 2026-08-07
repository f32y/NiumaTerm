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
mod tests {
    use super::*;

    #[test]
    fn live_and_persisted_metadata_parse_to_the_same_record() {
        let live = serde_json::json!({
            "type": "system", "subtype": "compact_boundary",
            "compact_metadata": {"trigger": "auto", "pre_tokens": 154_000,
                "post_tokens": 32_000, "messages_summarized": 87}});
        let persisted = serde_json::json!({
            "type": "system", "subtype": "compact_boundary",
            "compactMetadata": {"trigger": "auto", "preTokens": 154_000,
                "postTokens": 32_000, "messagesSummarized": 87}});

        let expected = Compaction {
            trigger: Some(CompactionTrigger::Automatic),
            pre_tokens: Some(154_000),
            post_tokens: Some(32_000),
            messages_summarized: Some(87),
            user_context: None,
            summary: None,
        };

        assert_eq!(parse_compaction(compaction_metadata(&live)), expected);
        assert_eq!(parse_compaction(compaction_metadata(&persisted)), expected);
    }

    #[test]
    fn unknown_trigger_and_blank_context_degrade_to_none() {
        let metadata = serde_json::json!({"trigger": "surprise", "user_context": "  "});
        let parsed = parse_compaction(&metadata);

        assert_eq!(parsed.trigger, None);
        assert_eq!(parsed.user_context, None);
        assert_eq!(parsed.pre_tokens, None);
    }

    #[test]
    fn manual_compaction_keeps_its_user_context() {
        let metadata = serde_json::json!({"trigger": "manual",
            "user_context": "keep the API design decisions"});
        let parsed = parse_compaction(&metadata);

        assert_eq!(parsed.trigger, Some(CompactionTrigger::Manual));
        assert_eq!(
            parsed.user_context.as_deref(),
            Some("keep the API design decisions")
        );
    }
}
