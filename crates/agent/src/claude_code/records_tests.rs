use crate::claude_code::records::*;

#[test]
fn model_notices_keep_content_and_accept_both_model_field_spellings() {
    for (original_key, fallback_key) in [
        ("original_model", "fallback_model"),
        ("originalModel", "fallbackModel"),
    ] {
        for subtype in [
            "model_refusal_fallback",
            "model_fallback",
            "model_consent_fallback",
        ] {
            let mut record = serde_json::json!({
                "type": "system", "subtype": subtype, "uuid": "notice",
            });

            record[original_key] = "first-model".into();
            record[fallback_key] = "second-model".into();

            assert!(matches!(
                model_notice(&record),
                Some(Item::Other { id, output: Some(output), status: Some(status), .. })
                    if id == "model-notice-notice"
                        && output == "Model changed from first-model to second-model."
                        && status == "completed"
            ));

            record["content"] = "\n  Provider explanation\n\nAdditional details. \n".into();

            assert!(matches!(
                model_notice(&record),
                Some(Item::Other { output: Some(output), .. })
                    if output == "\n  Provider explanation\n\nAdditional details. \n"
            ));
        }
    }

    assert!(
        model_notice(&serde_json::json!({
            "type": "system", "subtype": "unrecognized", "content": "ignore"
        }))
        .is_none()
    );
}

#[test]
fn model_restoration_notice_describes_the_original_model_as_current() {
    for (original_key, fallback_key) in [
        ("original_model", "fallback_model"),
        ("originalModel", "fallbackModel"),
    ] {
        let mut record = serde_json::json!({
            "type": "system", "subtype": "model_fallback", "direction": "revert"
        });

        record[original_key] = "first-model".into();
        record[fallback_key] = "second-model".into();

        assert!(matches!(model_notice(&record),
            Some(Item::Other { output: Some(output), .. })
                if output == "Model changed from second-model to first-model."
        ));
    }
}

#[test]
fn retracted_ids_combine_saved_and_streamed_fields_without_duplicates() {
    assert_eq!(
        retracted_message_uuids(&serde_json::json!({
            "retracted_message_uuids": ["first", "", 4],
            "retractedMessageUuids": ["second", "first"],
            "supersedes_uuids": ["third"],
            "supersedesUuids": ["second", "fourth"]
        })),
        vec!["first", "second", "third", "fourth"]
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
