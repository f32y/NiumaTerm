use crate::chat::Item;

#[test]
fn completed_items_merge_without_erasing_streamed_fields() {
    let mut command = Item::CommandExecution {
        id: "command-1".into(),
        command: "cargo test".into(),
        purpose: Some("Run focused tests".into()),
        aggregated_output: Some("streamed output".into()),
        status: Some("inProgress".into()),
        exit_code: None,
    };
    let completed = Item::CommandExecution {
        id: "command-1".into(),
        command: "cargo test".into(),
        purpose: None,
        aggregated_output: None,
        status: Some("completed".into()),
        exit_code: Some(0),
    };

    assert!(command.merge_completed(&completed));
    assert_eq!(
        command,
        Item::CommandExecution {
            id: "command-1".into(),
            command: "cargo test".into(),
            purpose: Some("Run focused tests".into()),
            aggregated_output: Some("streamed output".into()),
            status: Some("completed".into()),
            exit_code: Some(0),
        }
    );
}

#[test]
fn completed_reasoning_is_only_a_fallback_for_missing_stream_text() {
    let mut streamed = Item::Reasoning {
        id: "reasoning-1".into(),
        summary: Some("streamed".into()),
    };
    let completed = Item::Reasoning {
        id: "reasoning-1".into(),
        summary: Some("completed".into()),
    };
    assert!(streamed.merge_completed(&completed));
    assert_eq!(
        streamed,
        Item::Reasoning {
            id: "reasoning-1".into(),
            summary: Some("streamed".into())
        }
    );

    let mut missing = Item::Reasoning {
        id: "reasoning-1".into(),
        summary: None,
    };
    assert!(missing.merge_completed(&completed));
    assert_eq!(missing, completed);
}
