use std::sync::mpsc;

use serde_json::json;

use crate::codex::app_server::ThreadProfile;
use crate::codex::app_server::protocol::thread_name_request;
use crate::codex::app_server::title_generation::{
    TITLE_GENERATION_CANCEL_METHOD, TITLE_GENERATION_RESULT_METHOD, TitleGenerationHandle,
    TitleGenerationResult, generated_title_from_message, parse_title_generation_result,
    provisional_title_from_prompt, title_thread_start_request, title_turn_start_request,
};
use crate::workspace::AgentWorkspace;

#[test]
fn provisional_title_flattens_and_bounds_the_prompt() {
    assert_eq!(
        provisional_title_from_prompt("  Fix the login test\n  and retry handling  "),
        Some("Fix the login test and retry handling".to_string())
    );
    assert_eq!(provisional_title_from_prompt("/compact"), None);
    assert_eq!(provisional_title_from_prompt("   \n\t"), None);

    let long = "界".repeat(100);
    let title = provisional_title_from_prompt(&long).unwrap();
    assert_eq!(title.chars().count(), 60);
    assert!(title.ends_with('…'));
}

#[test]
fn internal_result_parser_keeps_generation_identity() {
    let result = parse_title_generation_result(
        TITLE_GENERATION_RESULT_METHOD,
        &json!({
            "generationId": 7,
            "rootThreadId": "thread-root",
            "provisionalTitle": "Opening prompt",
            "generatedTitle": "Inspect title updates",
        }),
    )
    .unwrap();

    assert_eq!(result.generation_id, 7);
    assert_eq!(result.root_thread_id, "thread-root");
    assert_eq!(result.provisional_title, "Opening prompt");
    assert_eq!(
        result.generated_title.as_deref(),
        Some("Inspect title updates")
    );
    assert!(parse_title_generation_result("other", &json!({})).is_none());
}

#[test]
fn generation_identity_rejects_stale_results() {
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let active = TitleGenerationHandle {
        generation_id: 7,
        root_thread_id: "thread-root".to_string(),
        provisional_title: "Opening prompt".to_string(),
        cancel_tx,
    };
    let current = TitleGenerationResult {
        generation_id: 7,
        root_thread_id: "thread-root".to_string(),
        provisional_title: "Opening prompt".to_string(),
        generated_title: Some("Inspect title updates".to_string()),
    };

    assert!(active.accepts(&current, Some("thread-root")));
    assert!(!active.accepts(&current, Some("thread-new")));
    let stale = TitleGenerationResult {
        generation_id: 6,
        ..current
    };
    assert!(!active.accepts(&stale, Some("thread-root")));

    active.cancel();
    assert_eq!(
        cancel_rx.recv().unwrap()["method"],
        TITLE_GENERATION_CANCEL_METHOD
    );
}

#[test]
fn generation_failure_resolves_to_the_provisional_title() {
    let generated = TitleGenerationResult {
        generation_id: 1,
        root_thread_id: "thread-root".to_string(),
        provisional_title: "Opening prompt".to_string(),
        generated_title: Some("Generated title".to_string()),
    };
    assert_eq!(generated.resolved_title(), "Generated title");

    let fallback = TitleGenerationResult {
        generated_title: None,
        ..generated
    };
    assert_eq!(fallback.resolved_title(), "Opening prompt");
    assert_eq!(
        thread_name_request(9, "thread-root", fallback.resolved_title()),
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "thread/name/set",
            "params": {"threadId": "thread-root", "name": "Opening prompt"},
        })
    );
}

#[test]
fn generated_title_parser_accepts_only_a_bounded_structured_title() {
    assert_eq!(
        generated_title_from_message(r#"{"title":"  Fix   login retries  "}"#),
        Some("Fix login retries".to_string())
    );
    assert_eq!(generated_title_from_message(r#"{"title":""}"#), None);
    assert_eq!(
        generated_title_from_message(&json!({"title": "x".repeat(37)}).to_string()),
        None
    );
    assert_eq!(generated_title_from_message("not json"), None);
}

#[test]
fn title_requests_are_ephemeral_read_only_and_structured() {
    let profile = ThreadProfile::default();
    let workspace = AgentWorkspace::new(Some(r"C:\workspace".to_string()), Vec::new());
    let start = title_thread_start_request(11, &profile, &workspace);

    assert_eq!(start["method"], "thread/start");
    assert_eq!(start["params"]["ephemeral"], true);
    assert_eq!(start["params"]["approvalPolicy"], "never");
    assert_eq!(start["params"]["model"], "gpt-5.6-luna");
    assert_eq!(start["params"]["config"]["web_search"], "disabled");
    assert_eq!(start["params"]["config"]["features.multi_agent"], false);

    let turn = title_turn_start_request(12, "thread-title", "Find the login parser");
    assert_eq!(turn["method"], "turn/start");
    assert_eq!(turn["params"]["threadId"], "thread-title");
    assert_eq!(turn["params"]["approvalPolicy"], "never");
    assert_eq!(turn["params"]["sandboxPolicy"]["type"], "readOnly");
    assert_eq!(turn["params"]["effort"], "low");
    assert_eq!(
        turn["params"]["outputSchema"]["properties"]["title"]["maxLength"],
        36
    );
    assert!(
        turn["params"]["input"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Find the login parser")
    );
}
