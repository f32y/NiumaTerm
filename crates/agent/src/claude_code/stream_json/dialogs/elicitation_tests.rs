use serde_json::{Value, json};

use crate::chat::{QuestionAnswer, QuestionInput};
use crate::claude_code::stream_json::dialogs::elicitation::Elicitation;

fn form(properties: Value, required: &[&str]) -> Value {
    json!({
        "subtype": "elicitation",
        "mcp_server_name": "test-server",
        "message": "Review the requested information.",
        "requested_schema": { "type": "object", "properties": properties, "required": required }
    })
}

fn selected(labels: &[&str]) -> QuestionAnswer {
    QuestionAnswer {
        selected: labels.iter().map(|label| (*label).to_owned()).collect(),
        custom: None,
    }
}

fn typed(value: &str) -> QuestionAnswer {
    QuestionAnswer {
        selected: Vec::new(),
        custom: Some(value.to_owned()),
    }
}

#[test]
fn form_preserves_order_and_submits_typed_values_without_optional_fields() {
    let mut request = form(
        json!({
            "name": { "type": "string", "minLength": 2 },
            "age": { "type": "integer", "minimum": 0, "maximum": 150 },
            "ratio": { "type": "number", "default": 0.5 },
            "enabled": { "type": "boolean" },
            "region": { "type": "string", "oneOf": [
                { "const": "us", "title": "United States" },
                { "const": "eu", "title": "Europe" }
            ] },
            "tags": { "type": "array", "minItems": 1, "items": { "type": "string", "enum": ["red", "blue"] } },
            "notes": { "type": "string", "default": "server suggestion" }
        }),
        &["name", "age", "enabled", "region", "tags"],
    );

    request["property_order"] =
        json!(["enabled", "name", "age", "ratio", "region", "tags", "notes"]);

    let elicitation = Elicitation::parse(&request).unwrap();

    assert_eq!(
        elicitation.questions()[0].header.as_deref(),
        Some("enabled")
    );
    assert_eq!(
        elicitation.questions()[0].input,
        QuestionInput::SelectionOnly
    );
    assert!(elicitation.questions()[0].question.contains("test-server"));
    assert!(elicitation.questions()[5].multi_select);

    let response = elicitation
        .respond(Some(&[
            selected(&["Yes"]),
            typed("Alice"),
            typed("35"),
            selected(&["Use default: 0.5"]),
            selected(&["Europe"]),
            selected(&["red", "blue"]),
            selected(&["Leave blank"]),
        ]))
        .unwrap();

    assert_eq!(
        response,
        json!({
            "action": "accept",
            "content": { "enabled": true, "name": "Alice", "age": 35, "ratio": 0.5, "region": "eu", "tags": ["red", "blue"] }
        })
    );
}

#[test]
fn form_decline_and_cancel_never_include_defaults_or_content() {
    let elicitation = Elicitation::parse(&form(
        json!({
            "answer": { "type": "string", "default": "suggestion" }
        }),
        &["answer"],
    ))
    .unwrap();

    assert_eq!(elicitation.decline(), json!({ "action": "decline" }));
    assert_eq!(
        elicitation.respond(None).unwrap(),
        json!({ "action": "cancel" })
    );
    assert!(elicitation.respond(Some(&[selected(&[])])).is_err());
    assert_eq!(
        elicitation
            .respond(Some(&[selected(&["Use default: suggestion"])]))
            .unwrap(),
        json!({
            "action": "accept", "content": { "answer": "suggestion" }
        })
    );
}

#[test]
fn empty_values_and_omitted_values_remain_distinct() {
    let elicitation = Elicitation::parse(&form(
        json!({
            "empty": { "type": "string" },
            "omitted": { "type": "string", "default": "unused" },
            "array": { "type": "array", "items": { "type": "string", "enum": ["a"] } }
        }),
        &["empty", "array"],
    ))
    .unwrap();

    assert_eq!(
        elicitation
            .respond(Some(&[
                selected(&["Use empty text"]),
                selected(&["Leave blank"]),
                selected(&["Select no values"]),
            ]))
            .unwrap(),
        json!({ "action": "accept", "content": { "empty": "", "array": [] } })
    );
}

#[test]
fn titled_array_values_and_defaults_use_schema_values_in_responses() {
    let elicitation = Elicitation::parse(&form(json!({
        "features": { "type": "array", "default": ["a"], "minItems": 1, "maxItems": 2, "items": {
            "anyOf": [ { "const": "a", "title": "Alpha" }, { "const": "b", "title": "Beta" } ]
        } }
    }), &["features"])).unwrap();

    assert_eq!(
        elicitation
            .respond(Some(&[selected(&["Alpha", "Beta"])]))
            .unwrap(),
        json!({
            "action": "accept", "content": { "features": ["a", "b"] }
        })
    );
    assert_eq!(
        elicitation
            .respond(Some(&[selected(&["Use default: a"])]))
            .unwrap(),
        json!({
            "action": "accept", "content": { "features": ["a"] }
        })
    );

    for answer in [
        selected(&["Alpha", "Use default: a"]),
        selected(&["Alpha", "Alpha"]),
        typed("Alpha"),
    ] {
        assert!(elicitation.respond(Some(&[answer])).is_err());
    }
}

#[test]
fn optional_array_special_choices_cannot_be_mixed_with_values() {
    let elicitation = Elicitation::parse(&form(json!({
        "features": { "type": "array", "maxItems": 1, "items": { "type": "string", "enum": ["a", "b"] } }
    }), &[])).unwrap();

    for labels in [
        &["a", "Leave blank"][..],
        &["a", "Select no values"][..],
        &["a", "b"][..],
    ] {
        assert!(elicitation.respond(Some(&[selected(labels)])).is_err());
    }

    assert_eq!(
        elicitation
            .respond(Some(&[selected(&["Leave blank"])]))
            .unwrap(),
        json!({ "action": "accept", "content": {} })
    );
}

#[test]
fn scalar_constraints_reject_invalid_answers() {
    for (schema, rejected, accepted) in [
        (
            json!({ "type": "integer", "minimum": 2, "maximum": 4 }),
            vec!["1", "5", "2.5", "true", "null", "NaN"],
            "3",
        ),
        (
            json!({ "type": "number", "minimum": 0.1, "maximum": 0.9 }),
            vec!["0", "1", "1e999", "\"0.5\""],
            "0.5",
        ),
        (
            json!({ "type": "string", "minLength": 2, "maxLength": 3 }),
            vec!["a", "abcd"],
            "🦀🦀",
        ),
        (
            json!({ "type": "string", "format": "email" }),
            vec!["plain", "a@", "a b@example.com", "a@@example.com"],
            "user+test@example.com",
        ),
        (
            json!({ "type": "string", "format": "uri" }),
            vec!["relative/path", "https://exa mple.com"],
            "https://example.com/path",
        ),
        (
            json!({ "type": "string", "format": "date" }),
            vec!["2026-02-29", "2026-2-01", "today"],
            "2024-02-29",
        ),
        (
            json!({ "type": "string", "format": "date-time" }),
            vec!["2026-01-01", "2026-01-01T12:00:00"],
            "2026-01-01T12:00:00+08:00",
        ),
    ] {
        let elicitation =
            Elicitation::parse(&form(json!({ "value": schema }), &["value"])).unwrap();

        for rejected in rejected {
            assert!(
                elicitation.respond(Some(&[typed(rejected)])).is_err(),
                "accepted {rejected:?}"
            );
        }

        assert!(
            elicitation.respond(Some(&[typed(accepted)])).is_ok(),
            "rejected {accepted:?}"
        );
    }
}

#[test]
fn selection_answers_cannot_inject_unknown_values_or_extra_text() {
    let elicitation = Elicitation::parse(&form(
        json!({
            "region": { "type": "string", "enum": ["a", "b"], "enumNames": ["Alpha", "Beta"] }
        }),
        &["region"],
    ))
    .unwrap();

    for answer in [
        selected(&["unknown"]),
        selected(&["Alpha", "Beta"]),
        typed("a"),
        QuestionAnswer {
            selected: vec!["Alpha".into()],
            custom: Some("b".into()),
        },
    ] {
        assert!(elicitation.respond(Some(&[answer])).is_err());
    }

    assert!(elicitation.respond(Some(&[])).is_err());
    assert!(
        elicitation
            .respond(Some(&[selected(&["Alpha"]), selected(&["Beta"])]))
            .is_err()
    );
    assert_eq!(
        elicitation.respond(Some(&[selected(&["Alpha"])])).unwrap()["content"]["region"],
        json!("a")
    );
}

#[test]
fn unsupported_or_ambiguous_schemas_are_rejected() {
    for schema in [
        json!({ "type": "object", "properties": {} }),
        json!({ "type": "string", "pattern": "a+" }),
        json!({ "type": "string", "format": "password" }),
        json!({ "type": "string", "minLength": -1 }),
        json!({ "type": "string", "minLength": 5, "maxLength": 2 }),
        json!({ "type": "number", "minimum": "1" }),
        json!({ "type": "number", "minimum": 5, "maximum": 2 }),
        json!({ "type": "boolean", "default": "yes" }),
        json!({ "type": "string", "minLength": 4, "default": "bad" }),
        json!({ "type": "string", "enum": [] }),
        json!({ "type": "string", "enum": ["a", "a"] }),
        json!({ "type": "string", "enum": ["a", "b"], "enumNames": ["Same", "Same"] }),
        json!({ "type": "string", "enum": ["a", "b"], "enumNames": ["One"] }),
        json!({ "type": "string", "enum": ["a"], "oneOf": [{ "const": "a", "title": "A" }] }),
        json!({ "type": "string", "oneOf": [{ "const": 1, "title": "A" }] }),
        json!({ "type": "string", "enum": ["Leave blank"] }),
        json!({ "type": "array", "items": { "type": "number" } }),
        json!({ "type": "array", "items": { "enum": ["a"] } }),
        json!({ "type": "array", "minItems": 2, "items": { "type": "string", "enum": ["a"] } }),
        json!({ "type": "array", "default": ["b"], "items": { "type": "string", "enum": ["a"] } }),
    ] {
        assert!(
            Elicitation::parse(&form(json!({ "value": schema.clone() }), &[])).is_err(),
            "accepted {schema}"
        );
    }
}

#[test]
fn malformed_property_order_required_fields_and_metadata_are_rejected() {
    let original = form(json!({ "name": { "type": "string" } }), &["name"]);

    for (key, value) in [
        ("property_order", json!(["missing"])),
        ("property_order", json!(["name", "name"])),
        ("property_order", json!([1])),
        ("title", json!(7)),
        ("mcp_server_name", json!("")),
        ("mode", json!("other")),
    ] {
        let mut request = original.clone();

        request[key] = value;

        assert!(Elicitation::parse(&request).is_err());
    }

    for required in [json!(["missing"]), json!(["name", "name"]), json!([1])] {
        let mut request = original.clone();

        request["requested_schema"]["required"] = required;

        assert!(Elicitation::parse(&request).is_err());
    }
}

#[test]
fn url_acceptance_is_consent_without_form_content() {
    let request = json!({
        "subtype": "elicitation", "mcp_server_name": "payment-server", "mode": "url",
        "message": "Review the external request.", "display_name": "Payments",
        "title": "Account access", "description": "The server needs website interaction.",
        "url": "https://example.com/authorize?token=abc", "elicitation_id": "flow-123"
    });

    let elicitation = Elicitation::parse(&request).unwrap();
    let question = &elicitation.questions()[0];

    assert_eq!(question.input, QuestionInput::SelectionOnly);

    for expected in [
        "payment-server",
        "Payments",
        "Account access",
        "Host: example.com",
        "https://example.com/authorize?token=abc",
        "does not confirm",
    ] {
        assert!(question.question.contains(expected), "missing {expected}");
    }

    assert_eq!(
        elicitation
            .respond(Some(&[selected(&["Allow this request"])]))
            .unwrap(),
        json!({ "action": "accept" })
    );
    assert_eq!(
        elicitation.respond(None).unwrap(),
        json!({ "action": "cancel" })
    );
    assert_eq!(elicitation.decline(), json!({ "action": "decline" }));
    assert!(elicitation.respond(Some(&[typed("yes")])).is_err());
    assert!(elicitation.respond(Some(&[selected(&[])])).is_err());

    for url in [
        "javascript:alert(1)",
        "file:///C:/secret",
        "relative/path",
        "https://example.com/\nother",
    ] {
        let mut invalid = request.clone();

        invalid["url"] = json!(url);

        assert!(Elicitation::parse(&invalid).is_err(), "accepted {url}");
    }

    let mut missing_id = request;

    missing_id.as_object_mut().unwrap().remove("elicitation_id");

    assert!(Elicitation::parse(&missing_id).is_err());
}

#[test]
fn empty_form_requires_confirmation_before_accepting() {
    let elicitation = Elicitation::parse(&form(json!({}), &[])).unwrap();

    assert_eq!(elicitation.questions().len(), 1);
    assert!(elicitation.respond(Some(&[])).is_err());
    assert!(elicitation.respond(Some(&[selected(&[])])).is_err());
    assert_eq!(
        elicitation
            .respond(Some(&[selected(&["Submit empty form"])]))
            .unwrap(),
        json!({ "action": "accept", "content": {} })
    );
}

#[test]
fn server_markdown_cannot_hide_url_details_and_url_backslashes_are_encoded() {
    let request = json!({
        "mcp_server_name": "test-server", "mode": "url", "elicitation_id": "review",
        "message": "<!-- hidden section\n```", "url": "https://example.com/path?value=\\"
    });

    let elicitation = Elicitation::parse(&request).unwrap();
    let prompt = &elicitation.questions()[0].question;

    assert!(prompt.contains("\\<\\!-- hidden section"));
    assert!(prompt.contains("\\`\\`\\`"));
    assert!(prompt.contains("Host: example.com"));
    assert!(prompt.contains("(<https://example.com/path?value=%5C>)"));
}
