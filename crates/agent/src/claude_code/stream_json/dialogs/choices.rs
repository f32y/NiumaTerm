#[cfg(test)]
#[path = "choices_tests.rs"]
mod tests;

use serde_json::{Value, json};

use crate::chat::{Question, QuestionAnswer, QuestionInput, QuestionOption};

pub(super) const SUPPORTED_KINDS: &[&str] = &[
    "refusal_fallback_prompt",
    "fable_overage_consent_prompt",
    "auto_mode_server_fallback",
    "device_mcp_consent",
    "unattended_serving_consent",
];

pub(super) struct ChoiceDialog {
    questions: [Question; 1],
    results: Vec<&'static str>,
    retracted_messages: Vec<String>,
}

pub(super) struct ChoiceResponse {
    pub(super) response: Value,
    pub(super) edit_prompt: bool,
}

impl ChoiceDialog {
    pub(super) fn parse(kind: &str, payload: &Value) -> Result<Option<Self>, String> {
        let dialog = match kind {
            "refusal_fallback_prompt" => refusal(payload)?,
            "fable_overage_consent_prompt" => usage_credits(payload)?,
            "auto_mode_server_fallback" => auto_mode(payload)?,
            "device_mcp_consent" => device_mcp(payload)?,
            "unattended_serving_consent" => unattended(payload)?,
            _ => return Ok(None),
        };

        Ok(Some(dialog))
    }

    fn new(header: &str, question: String, choices: &[(&str, &str, &'static str)]) -> Self {
        Self {
            questions: [Question {
                header: Some(header.to_owned()),
                question,
                multi_select: false,
                options: choices
                    .iter()
                    .map(|(label, description, _)| QuestionOption {
                        label: (*label).to_owned(),
                        description: Some((*description).to_owned()),
                    })
                    .collect(),
                input: QuestionInput::SelectionOnly,
            }],
            results: choices.iter().map(|(_, _, result)| *result).collect(),
            retracted_messages: Vec::new(),
        }
    }

    pub(super) fn questions(&self) -> &[Question] {
        &self.questions
    }

    pub(super) fn retracted_messages(&self) -> &[String] {
        &self.retracted_messages
    }

    pub(super) fn respond(
        &self,
        answers: Option<&[QuestionAnswer]>,
    ) -> Result<ChoiceResponse, String> {
        let Some(answers) = answers else {
            return Ok(ChoiceResponse {
                response: json!({"behavior": "cancelled"}),
                edit_prompt: false,
            });
        };

        let [answer] = answers else {
            return Err("choose exactly one dialog option".into());
        };

        if answer.custom.is_some() {
            return Err("this dialog accepts a listed option only".into());
        }

        let [selected] = answer.selected.as_slice() else {
            return Err("choose exactly one dialog option".into());
        };

        let index = self.questions[0]
            .options
            .iter()
            .position(|option| option.label == *selected)
            .ok_or("the selected dialog option is unavailable")?;

        let result = self.results[index];

        Ok(ChoiceResponse {
            response: json!({"behavior": "completed", "result": result}),
            edit_prompt: result == "edit_prompt",
        })
    }
}

fn refusal(payload: &Value) -> Result<ChoiceDialog, String> {
    let original = text(payload, "originalModel")?;
    let fallback = text(payload, "fallbackModel")?;
    let category = nullable_text(payload, "apiRefusalCategory")?;
    let guidance = optional_text(payload, "guidanceText")?;

    let retracted = match payload.get("retractedMessageUuids") {
        Some(_) => texts(payload, "retractedMessageUuids")?
            .into_iter()
            .map(str::to_owned)
            .collect(),
        None => Vec::new(),
    };

    let mut question = format!(
        "Safeguards stopped the response from {original}.\n\n\
         Original model: {original}\n\nFallback model: {fallback}"
    );

    if let Some(category) = category {
        question.push_str(&format!("\n\nReason: {category}"));
    }

    if let Some(guidance) = guidance {
        question.push_str(&format!("\n\n{guidance}"));
    }

    let mut dialog = ChoiceDialog::new(
        "Safeguards",
        question,
        &[
            (
                "Continue with the fallback model",
                "Claude will retry the request using the model shown above.",
                "retry_fallback",
            ),
            (
                "Edit the prompt and retry",
                "Return the original prompt to the editor so you can change it before retrying.",
                "edit_prompt",
            ),
            (
                "Cancel this request",
                "End this request without retrying it on the fallback model.",
                "cancelled",
            ),
        ],
    );

    dialog.retracted_messages = retracted;

    Ok(dialog)
}

fn usage_credits(payload: &Value) -> Result<ChoiceDialog, String> {
    let enabled = boolean(payload, "overagesEnabled")?;
    let model_name = optional_text(payload, "modelName")?;
    let model_id = optional_text(payload, "model")?;
    let currency = nullable_text(payload, "currency")?;

    let balance = match payload.get("balanceCents") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_f64()
                .ok_or("dialog field balanceCents must be a number or null")?,
        ),
    };

    let model = model_name.or(model_id).unwrap_or("This model");

    let mut question =
        format!("Continuing with {model} uses usage credits purchased separately from your plan.");

    if let Some(model_id) = model_id {
        question.push_str(&format!("\n\nModel: {model_id}"));
    }

    question.push_str(if enabled {
        "\n\nUsage credits are enabled."
    } else {
        "\n\nUsage credits are disabled. Claude may require account setup before continuing."
    });

    match (balance, currency) {
        (Some(cents), Some(currency)) => {
            question.push_str(&format!(
                "\n\nAvailable balance: {currency} {:.2}",
                cents / 100.0
            ));
        }
        (Some(cents), None) => {
            question.push_str(&format!("\n\nAvailable balance: {cents} cents"));
        }
        (None, Some(currency)) => question.push_str(&format!("\n\nCurrency: {currency}")),
        (None, None) => {}
    }

    Ok(ChoiceDialog::new(
        "Usage credits",
        question,
        &[
            (
                "Continue using usage credits",
                "Allow Claude to use separately purchased credits for this model.",
                "consent",
            ),
            (
                "Switch to the default model",
                "Claude chooses the model and may also save it as your default, depending on your configuration.",
                "switch_default",
            ),
            (
                "Dismiss",
                "Claude may continue this turn using its default model.",
                "cancelled",
            ),
        ],
    ))
}

fn auto_mode(payload: &Value) -> Result<ChoiceDialog, String> {
    let title = text(payload, "title")?;
    let paragraphs = texts(payload, "paragraphs")?;
    let help_url = text(payload, "helpUrl")?;
    let gateway = optional_text(payload, "gatewayHost")?;

    let mut question = title.to_owned();

    for paragraph in paragraphs {
        question.push_str(&format!("\n\n{paragraph}"));
    }

    if let Some(gateway) = gateway {
        question.push_str(&format!("\n\nGateway: {gateway}"));
    }

    question.push_str(&format!("\n\n[Learn more]({help_url})"));

    Ok(ChoiceDialog::new(
        "Auto mode",
        question,
        &[
            (
                "Continue in Auto mode",
                "Acknowledge the classifier billing notice and continue.",
                "continue",
            ),
            ("Stop this turn", "Interrupt the current turn.", "interrupt"),
        ],
    ))
}

fn device_mcp(payload: &Value) -> Result<ChoiceDialog, String> {
    if integer(payload, "v")? != 1 {
        return Err("unsupported device MCP consent version".into());
    }

    let machine = bounded_text(payload, "machine_name", 256)?;
    let reconsent = boolean(payload, "reconsent")?;
    let servers = array(payload, "servers")?;

    if servers.is_empty() || servers.len() > 64 {
        return Err("device MCP consent must include between 1 and 64 servers".into());
    }

    let mut question = format!(
        "Allow remote sessions to use MCP tools on {machine}?\n\n\
         Review the servers and tools included in this request."
    );

    if reconsent {
        question.push_str("\n\nThis request renews previously recorded consent.");
    }

    if payload.get("changed_servers").is_some() {
        let changed = texts(payload, "changed_servers")?;

        for name in &changed {
            validate_length(name, "changed_servers entry", 128)?;
        }

        if !changed.is_empty() {
            question.push_str("\n\nChanged servers:\n");

            for name in changed {
                question.push_str(&format!("\n- {name}"));
            }
        }
    }

    for server in servers {
        append_server(&mut question, server)?;
    }

    Ok(ChoiceDialog::new(
        "Device MCP access",
        question,
        &[
            (
                "Allow access",
                "Approve access to the listed MCP servers.",
                "allow",
            ),
            (
                "Deny access",
                "Reject access to the listed MCP servers.",
                "deny",
            ),
            ("Not now", "Decide later.", "not_now"),
        ],
    ))
}

fn append_server(question: &mut String, server: &Value) -> Result<(), String> {
    let name = bounded_text(server, "name", 128)?;
    let scope = text(server, "scope")?;
    let transport = text(server, "transport")?;
    let count = integer(server, "tool_count")?;
    let names = texts(server, "tool_names")?;
    let truncated = boolean(server, "tool_names_truncated")?;

    if !matches!(scope, "user" | "local" | "plugin") {
        return Err("unsupported MCP server scope".into());
    }

    if !matches!(transport, "stdio" | "http" | "sse" | "ws") {
        return Err("unsupported MCP server transport".into());
    }

    if names.len() > 20 {
        return Err("an MCP server summary must include at most 20 tool names".into());
    }

    for tool in &names {
        validate_length(tool, "tool_names entry", 128)?;
    }

    question.push_str(&format!(
        "\n\n### {name}\n\nScope: {scope}\n\nConnection: {transport}\n\nTool count: {count}"
    ));

    if !names.is_empty() {
        question.push('\n');
    }

    for tool in names {
        question.push_str(&format!("\n- {tool}"));
    }

    if truncated {
        question.push_str("\n\nOnly part of the tool list is included in this request.");
    }

    Ok(())
}

fn unattended(payload: &Value) -> Result<ChoiceDialog, String> {
    let machine = text(payload, "machineName")?;
    let title = text(payload, "title")?;
    let body = text(payload, "body")?;
    let detail = text(payload, "detail")?;
    let terms = text(payload, "terms")?;
    let version = integer(payload, "version")?;

    Ok(ChoiceDialog::new(
        "Unattended access",
        format!(
            "{title}\n\nMachine: {machine}\n\n{body}\n\n{detail}\n\n{terms}\n\nTerms version: {version}"
        ),
        &[
            (
                "Allow unattended access",
                "Approve the access described above.",
                "accept",
            ),
            (
                "Decline",
                "Reject unattended access on this machine.",
                "decline",
            ),
            ("Not now", "Decide later.", "not_now"),
        ],
    ))
}

fn text<'a>(payload: &'a Value, field: &str) -> Result<&'a str, String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("dialog field {field} must be a string"))
}

fn optional_text<'a>(payload: &'a Value, field: &str) -> Result<Option<&'a str>, String> {
    match payload.get(field) {
        None => Ok(None),
        Some(_) => text(payload, field).map(Some),
    }
}

fn nullable_text<'a>(payload: &'a Value, field: &str) -> Result<Option<&'a str>, String> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => text(payload, field).map(Some),
    }
}

fn array<'a>(payload: &'a Value, field: &str) -> Result<&'a [Value], String> {
    payload
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("dialog field {field} must be an array"))
}

fn texts<'a>(payload: &'a Value, field: &str) -> Result<Vec<&'a str>, String> {
    array(payload, field)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| format!("dialog field {field} must contain strings"))
        })
        .collect()
}

fn boolean(payload: &Value, field: &str) -> Result<bool, String> {
    payload
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("dialog field {field} must be a boolean"))
}

fn integer(payload: &Value, field: &str) -> Result<u64, String> {
    payload
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("dialog field {field} must be a nonnegative integer"))
}

fn bounded_text<'a>(payload: &'a Value, field: &str, maximum: usize) -> Result<&'a str, String> {
    let value = text(payload, field)?;

    validate_length(value, field, maximum)?;

    Ok(value)
}

fn validate_length(value: &str, field: &str, maximum: usize) -> Result<(), String> {
    let length = value.encode_utf16().count();

    if length == 0 || length > maximum {
        return Err(format!(
            "dialog field {field} must contain between 1 and {maximum} characters"
        ));
    }

    Ok(())
}
