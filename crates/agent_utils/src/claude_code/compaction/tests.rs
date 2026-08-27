use crate::claude_code::compaction::*;

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
