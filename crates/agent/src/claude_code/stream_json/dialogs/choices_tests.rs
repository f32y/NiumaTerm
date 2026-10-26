use serde_json::{Value, json};

use crate::chat::{QuestionAnswer, QuestionInput};
use crate::claude_code::stream_json::dialogs::choices::{ChoiceDialog, SUPPORTED_KINDS};

fn refusal_payload() -> Value {
    json!({
        "originalModel": "claude-opus-5-5[1m]",
        "fallbackModel": "claude-opus-5",
        "apiRefusalCategory": "reasoning_extraction",
        "guidanceText": "Review the prompt before continuing.",
        "retractedMessageUuids": ["partial-one", "partial-two"]
    })
}

fn device_payload() -> Value {
    json!({
        "v": 1,
        "machine_name": "Build machine",
        "reconsent": true,
        "changed_servers": ["Deploy tools"],
        "servers": [{
            "name": "Deploy tools",
            "scope": "user",
            "transport": "stdio",
            "tool_count": 4,
            "tool_names": ["list_deployments", "publish_release"],
            "tool_names_truncated": true
        }, {
            "name": "Reports",
            "scope": "plugin",
            "transport": "http",
            "tool_count": 1,
            "tool_names": ["read_report"],
            "tool_names_truncated": false
        }]
    })
}

fn unattended_payload() -> Value {
    json!({
        "machineName": "Office desktop",
        "title": "Allow access while you are away?",
        "body": "Remote sessions can use this machine.",
        "detail": "Review the allowed resources before accepting.",
        "terms": "Access remains enabled until you change this preference.",
        "version": 3
    })
}

fn choose(dialog: &ChoiceDialog, index: usize) -> QuestionAnswer {
    QuestionAnswer {
        selected: vec![dialog.questions()[0].options[index].label.clone()],
        custom: None,
    }
}

#[test]
fn refusal_choices_preserve_protocol_results_and_edit_intent() {
    let dialog = ChoiceDialog::parse("refusal_fallback_prompt", &refusal_payload())
        .unwrap()
        .unwrap();

    assert_eq!(dialog.retracted_messages(), ["partial-one", "partial-two"]);

    for (index, result) in ["retry_fallback", "edit_prompt", "cancelled"]
        .into_iter()
        .enumerate()
    {
        let response = dialog.respond(Some(&[choose(&dialog, index)])).unwrap();

        assert_eq!(
            response.response,
            json!({"behavior": "completed", "result": result})
        );
        assert_eq!(response.edit_prompt, result == "edit_prompt");
    }

    let question = &dialog.questions()[0];

    assert_eq!(question.input, QuestionInput::SelectionOnly);
    assert!(!question.multi_select);
    assert!(question.question.contains("claude-opus-5-5[1m]"));
    assert!(question.question.contains("reasoning_extraction"));
    assert!(question.question.contains("Review the prompt"));
}

#[test]
fn every_supported_dialog_requires_a_user_choice() {
    let cases = [
        ("refusal_fallback_prompt", refusal_payload()),
        (
            "fable_overage_consent_prompt",
            json!({"overagesEnabled": true}),
        ),
        (
            "auto_mode_server_fallback",
            json!({"title": "Billing notice", "paragraphs": [], "helpUrl": "https://example.com/help"}),
        ),
        ("device_mcp_consent", device_payload()),
        ("unattended_serving_consent", unattended_payload()),
    ];

    assert_eq!(cases.len(), SUPPORTED_KINDS.len());

    for (kind, payload) in cases {
        let dialog = ChoiceDialog::parse(kind, &payload).unwrap().unwrap();
        let response = dialog.respond(None).unwrap();

        assert_eq!(response.response, json!({"behavior": "cancelled"}));
        assert!(!response.edit_prompt);
        assert!(dialog.respond(Some(&[])).is_err());
        assert!(
            dialog
                .respond(Some(&[QuestionAnswer {
                    selected: Vec::new(),
                    custom: None,
                }]))
                .is_err()
        );
    }
}

#[test]
fn dialog_answers_reject_unknown_multiple_and_custom_values() {
    let dialog = ChoiceDialog::parse("refusal_fallback_prompt", &refusal_payload())
        .unwrap()
        .unwrap();

    let first = choose(&dialog, 0);

    for answers in [
        vec![QuestionAnswer {
            selected: vec!["retry_fallback".into()],
            custom: None,
        }],
        vec![QuestionAnswer {
            selected: vec![first.selected[0].clone(), "Cancel this request".into()],
            custom: None,
        }],
        vec![QuestionAnswer {
            selected: first.selected.clone(),
            custom: Some(String::new()),
        }],
        vec![first.clone(), first],
    ] {
        assert!(dialog.respond(Some(&answers)).is_err());
    }
}

#[test]
fn usage_credit_choices_explain_cost_and_default_model_behavior() {
    let dialog = ChoiceDialog::parse(
        "fable_overage_consent_prompt",
        &json!({
            "overagesEnabled": true,
            "modelName": "Fable 5.1",
            "model": "claude-fable-5-1",
            "balanceCents": 1250,
            "currency": "USD"
        }),
    )
    .unwrap()
    .unwrap();

    let question = &dialog.questions()[0];

    assert!(question.question.contains("purchased separately"));
    assert!(question.question.contains("USD 12.50"));
    assert!(question.question.contains("claude-fable-5-1"));
    assert!(
        question.options[1]
            .description
            .as_ref()
            .unwrap()
            .contains("save")
    );
    assert!(
        question.options[2]
            .description
            .as_ref()
            .unwrap()
            .contains("may continue")
    );

    for (index, result) in ["consent", "switch_default", "cancelled"]
        .into_iter()
        .enumerate()
    {
        let response = dialog.respond(Some(&[choose(&dialog, index)])).unwrap();

        assert_eq!(response.response["result"], result);
        assert!(!response.edit_prompt);
    }
}

#[test]
fn auto_mode_notice_preserves_copy_and_interrupt_choice() {
    let dialog = ChoiceDialog::parse(
        "auto_mode_server_fallback",
        &json!({
            "gatewayHost": "proxy.example.com",
            "title": "Classifier requests remain billable",
            "paragraphs": ["Auto mode can continue.", "Your gateway needs an update."],
            "helpUrl": "https://example.com/classifier-billing"
        }),
    )
    .unwrap()
    .unwrap();

    let question = &dialog.questions()[0].question;

    for content in [
        "Classifier requests remain billable",
        "Auto mode can continue.",
        "Your gateway needs an update.",
        "proxy.example.com",
        "https://example.com/classifier-billing",
    ] {
        assert!(question.contains(content));
    }

    assert_eq!(
        dialog
            .respond(Some(&[choose(&dialog, 0)]))
            .unwrap()
            .response["result"],
        "continue"
    );
    assert_eq!(
        dialog
            .respond(Some(&[choose(&dialog, 1)]))
            .unwrap()
            .response["result"],
        "interrupt"
    );
}

#[test]
fn device_consent_includes_every_server_and_available_tool_detail() {
    let dialog = ChoiceDialog::parse("device_mcp_consent", &device_payload())
        .unwrap()
        .unwrap();

    let question = &dialog.questions()[0].question;

    for content in [
        "Build machine",
        "renews previously recorded consent",
        "Changed servers:",
        "Deploy tools",
        "Reports",
        "Scope: user",
        "Scope: plugin",
        "Connection: stdio",
        "Connection: http",
        "Tool count: 4",
        "list_deployments",
        "publish_release",
        "read_report",
        "Only part of the tool list",
    ] {
        assert!(
            question.contains(content),
            "missing consent detail: {content}"
        );
    }

    for (index, result) in ["allow", "deny", "not_now"].into_iter().enumerate() {
        assert_eq!(
            dialog
                .respond(Some(&[choose(&dialog, index)]))
                .unwrap()
                .response["result"],
            result
        );
    }
}

#[test]
fn unattended_consent_includes_terms_and_has_separate_deferral() {
    let payload = unattended_payload();

    let dialog = ChoiceDialog::parse("unattended_serving_consent", &payload)
        .unwrap()
        .unwrap();

    for field in ["machineName", "title", "body", "detail", "terms"] {
        assert!(
            dialog.questions()[0]
                .question
                .contains(payload[field].as_str().unwrap())
        );
    }

    for (index, result) in ["accept", "decline", "not_now"].into_iter().enumerate() {
        assert_eq!(
            dialog
                .respond(Some(&[choose(&dialog, index)]))
                .unwrap()
                .response["result"],
            result
        );
    }
}

#[test]
fn malformed_payloads_do_not_produce_a_consent_card() {
    let mut missing_terms = unattended_payload();

    missing_terms.as_object_mut().unwrap().remove("terms");

    let mut wrong_version = device_payload();

    wrong_version["v"] = json!(2);

    let mut incomplete_server = device_payload();

    incomplete_server["servers"][0]
        .as_object_mut()
        .unwrap()
        .remove("tool_names_truncated");

    let mut invalid_name = device_payload();

    invalid_name["servers"][1]["name"] = json!("x".repeat(129));

    let mut invalid_transport = device_payload();

    invalid_transport["servers"][1]["transport"] = json!("https");

    for (kind, payload) in [
        ("refusal_fallback_prompt", json!({"originalModel": "opus"})),
        (
            "refusal_fallback_prompt",
            json!({"originalModel": "opus", "fallbackModel": "sonnet", "retractedMessageUuids": [1]}),
        ),
        (
            "fable_overage_consent_prompt",
            json!({"overagesEnabled": "yes"}),
        ),
        (
            "fable_overage_consent_prompt",
            json!({"overagesEnabled": false, "balanceCents": "100"}),
        ),
        (
            "auto_mode_server_fallback",
            json!({"title": "Billing", "paragraphs": [3], "helpUrl": "https://example.com"}),
        ),
        ("device_mcp_consent", invalid_transport),
        ("device_mcp_consent", wrong_version),
        ("device_mcp_consent", incomplete_server),
        ("device_mcp_consent", invalid_name),
        ("unattended_serving_consent", missing_terms),
    ] {
        assert!(ChoiceDialog::parse(kind, &payload).is_err(), "{kind}");
    }
}

#[test]
fn reserved_and_local_dialogs_are_not_advertised_or_parsed() {
    for kind in [
        "enum_prompt",
        "local_jsx",
        "permission_bash",
        "future_dialog",
    ] {
        assert!(!SUPPORTED_KINDS.contains(&kind));
        assert!(ChoiceDialog::parse(kind, &json!({})).unwrap().is_none());
    }
}

#[test]
fn optional_details_can_be_absent_or_extended() {
    let dialog = ChoiceDialog::parse(
        "fable_overage_consent_prompt",
        &json!({"overagesEnabled": false, "balanceCents": null, "currency": null, "futureField": {"v": 1}}),
    )
    .unwrap()
    .unwrap();

    assert!(
        dialog.questions()[0]
            .question
            .contains("Usage credits are disabled")
    );

    let dialog = ChoiceDialog::parse(
        "refusal_fallback_prompt",
        &json!({"originalModel": "opus", "fallbackModel": "sonnet", "apiRefusalCategory": null}),
    )
    .unwrap()
    .unwrap();

    assert!(dialog.retracted_messages().is_empty());
}
