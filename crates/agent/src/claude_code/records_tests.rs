use crate::claude_code::records::*;

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

/// A child's conversation decodes server-side tool calls like the parent's,
/// and a message wrapping an API failure reads as an error.
#[test]
fn child_content_keeps_server_tools_and_api_errors() {
    let mut open_tools = HashMap::new();

    let tool = serde_json::json!({
        "type": "assistant", "uuid": "a1",
        "message": {"content": [
            {"type": "text", "text": "\n\nSearching"},
            {"type": "server_tool_use", "id": "srv", "name": "web_search", "input": {"query": "x"}},
        ]},
    });

    let items = child_content_items(&tool, &mut open_tools);

    assert!(
        matches!(&items[0], Item::AgentMessage { text: Some(text), .. } if text == "Searching")
    );
    assert_eq!(items[1].id(), Some("srv"));
    assert!(open_tools.contains_key("srv"));

    let failure = serde_json::json!({
        "type": "assistant", "uuid": "a2", "isApiErrorMessage": true,
        "message": {"content": [{"type": "text", "text": "API Error: overloaded"}]},
    });

    assert_eq!(
        child_content_items(&failure, &mut open_tools),
        vec![Item::Error {
            text: "API Error: overloaded".into(),
        }]
    );
}
