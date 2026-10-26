#[cfg(test)]
#[path = "elicitation_tests.rs"]
mod tests;

use std::collections::HashSet;

use chrono::{DateTime, NaiveDate};
use reqwest::Url;
use serde_json::{Map, Value, json};

use crate::chat::{Question, QuestionAnswer, QuestionInput, QuestionOption};

const URL_CONSENT: &str = "Allow this request";
const EMPTY_FORM_CONSENT: &str = "Submit empty form";
const LEAVE_BLANK: &str = "Leave blank";

pub(super) struct Elicitation {
    questions: Vec<Question>,
    fields: Option<Vec<FormField>>,
}

struct FormField {
    name: String,
    required: bool,
    kind: FieldKind,
    choices: Vec<FieldChoice>,
}

struct FieldChoice {
    label: String,
    value: Option<Value>,
}

enum FieldKind {
    String {
        minimum: Option<u64>,
        maximum: Option<u64>,
        format: Option<StringFormat>,
    },
    Number {
        integer: bool,
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Boolean,
    Enum(Vec<String>),
    Array {
        values: Vec<String>,
        minimum: Option<u64>,
        maximum: Option<u64>,
    },
}

enum StringFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

impl Elicitation {
    pub(super) fn parse(request: &Value) -> Result<Self, String> {
        let request = object(request, "elicitation request")?;
        let server = text(request, "mcp_server_name")?;

        if server.trim().is_empty() {
            return Err("Elicitation requires an MCP server name".into());
        }

        let message = text(request, "message")?;

        let mut introduction = format!("MCP server: {}", markdown_text(server));

        for name in ["display_name", "title", "description"] {
            if let Some(value) = optional_text(request, name)? {
                introduction.push_str(&format!("\n\n{}", markdown_text(value)));
            }
        }

        introduction.push_str(&format!("\n\n{}", markdown_text(message)));

        match optional_text(request, "mode")?.unwrap_or("form") {
            "form" => Self::parse_form(request, &introduction),
            "url" => {
                let url_text = text(request, "url")?;
                let url = Url::parse(url_text).map_err(|_| "Invalid elicitation URL")?;

                if !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || url_text.chars().any(char::is_whitespace)
                    || url_text.chars().any(char::is_control)
                {
                    return Err("Elicitation URL must be an HTTP or HTTPS address".into());
                }

                if text(request, "elicitation_id")?.trim().is_empty() {
                    return Err("URL elicitation requires an elicitation ID".into());
                }

                let prompt = format!(
                    "{introduction}\n\nHost: {}\n\nURL: [{}](<{}>)\n\nAllow this MCP server to request interaction at this website. Consent does not confirm that the website interaction is complete.",
                    markdown_text(url.host_str().unwrap_or_default()),
                    markdown_text(url_text),
                    url.as_str().replace('\\', "%5C"),
                );

                Ok(Self {
                    questions: vec![confirmation_question(prompt, URL_CONSENT)],
                    fields: None,
                })
            }
            _ => Err("Unsupported elicitation mode".into()),
        }
    }

    fn parse_form(request: &Map<String, Value>, introduction: &str) -> Result<Self, String> {
        let schema = object(
            request
                .get("requested_schema")
                .ok_or("Form elicitation requires requested_schema")?,
            "requested_schema",
        )?;

        check_keys(
            schema,
            &[
                "type",
                "properties",
                "required",
                "title",
                "description",
                "$schema",
            ],
        )?;

        if text(schema, "type")? != "object" {
            return Err("Elicitation form schema must have type object".into());
        }

        for name in ["title", "description", "$schema"] {
            optional_text(schema, name)?;
        }

        let properties = object(
            schema.get("properties").ok_or("Missing form properties")?,
            "form properties",
        )?;

        let required = optional_strings(schema, "required")?.unwrap_or_default();

        let mut unique = HashSet::new();

        for name in &required {
            if !properties.contains_key(name) || !unique.insert(name) {
                return Err("Required fields must name distinct form properties".into());
            }
        }

        let mut order = optional_strings(request, "property_order")?.unwrap_or_default();

        unique.clear();

        for name in &order {
            if !properties.contains_key(name) || !unique.insert(name) {
                return Err("Property order must name distinct form properties".into());
            }
        }

        let remaining: Vec<String> = properties
            .keys()
            .filter(|name| !unique.contains(name))
            .cloned()
            .collect();

        order.extend(remaining);

        let mut fields = Vec::with_capacity(order.len());
        let mut questions = Vec::with_capacity(order.len());

        for name in order {
            let schema = object(&properties[&name], "field schema")?;

            let (field, mut question) =
                FormField::parse(name.clone(), schema, required.contains(&name))
                    .map_err(|error| format!("{name}: {error}"))?;

            if questions.is_empty() {
                question.question = format!("{introduction}\n\n{}", question.question);
            }

            fields.push(field);
            questions.push(question);
        }

        if questions.is_empty() {
            questions.push(confirmation_question(
                format!("{introduction}\n\nThe server requested a form with no fields."),
                EMPTY_FORM_CONSENT,
            ));
        }

        Ok(Self {
            questions,
            fields: Some(fields),
        })
    }

    pub(super) fn questions(&self) -> &[Question] {
        &self.questions
    }

    pub(super) fn respond(&self, answers: Option<&[QuestionAnswer]>) -> Result<Value, String> {
        let Some(answers) = answers else {
            return Ok(json!({ "action": "cancel" }));
        };

        if answers.len() != self.questions.len() {
            return Err("Answer count does not match the elicitation form".into());
        }

        let Some(fields) = &self.fields else {
            validate_confirmation(&answers[0], URL_CONSENT)?;

            return Ok(json!({ "action": "accept" }));
        };

        let mut content = Map::new();

        if fields.is_empty() {
            validate_confirmation(&answers[0], EMPTY_FORM_CONSENT)?;
        }

        for (field, answer) in fields.iter().zip(answers) {
            if let Some(value) = field
                .answer(answer)
                .map_err(|error| format!("{}: {error}", field.name))?
            {
                content.insert(field.name.clone(), value);
            }
        }

        Ok(json!({ "action": "accept", "content": content }))
    }

    pub(super) fn decline(&self) -> Value {
        json!({ "action": "decline" })
    }
}

impl FormField {
    fn parse(
        name: String,
        schema: &Map<String, Value>,
        required: bool,
    ) -> Result<(Self, Question), String> {
        if name.is_empty() {
            return Err("Form property name cannot be empty".into());
        }

        let title = optional_text(schema, "title")?.unwrap_or(&name);
        let description = optional_text(schema, "description")?;

        let (kind, mut choices) = FieldKind::parse(schema)?;

        if let Some(default) = schema.get("default") {
            kind.validate(default)?;

            choices.push(FieldChoice {
                label: format!("Use default: {}", display_value(default)),
                value: Some(default.clone()),
            });
        }

        if !required {
            choices.push(FieldChoice {
                label: LEAVE_BLANK.into(),
                value: None,
            });
        }

        let mut labels = HashSet::new();

        if choices
            .iter()
            .any(|choice| choice.label.trim().is_empty() || !labels.insert(choice.label.as_str()))
        {
            return Err("Form choices have empty or duplicate labels".into());
        }

        let mut prompt = format!(
            "{} ({})\n\n{}",
            markdown_text(title),
            if required { "required" } else { "optional" },
            kind.instructions(),
        );

        if let Some(description) = description {
            prompt.push_str(&format!("\n\n{}", markdown_text(description)));
        }

        let question = Question {
            header: Some(title.to_owned()),
            question: prompt,
            multi_select: matches!(kind, FieldKind::Array { .. }),
            options: choices
                .iter()
                .map(|choice| QuestionOption {
                    label: choice.label.clone(),
                    description: choice.value.is_none().then(|| "Omit this field".into()),
                })
                .collect(),
            input: if matches!(kind, FieldKind::String { .. } | FieldKind::Number { .. }) {
                QuestionInput::Text
            } else {
                QuestionInput::SelectionOnly
            },
        };

        Ok((
            Self {
                name,
                required,
                kind,
                choices,
            },
            question,
        ))
    }

    fn answer(&self, answer: &QuestionAnswer) -> Result<Option<Value>, String> {
        let value = if let Some(custom) = &answer.custom {
            if !answer.selected.is_empty() {
                return Err("Choose an option or enter text, without combining both".into());
            }

            Some(match &self.kind {
                FieldKind::String { .. } => Value::String(custom.clone()),
                FieldKind::Number { .. } => {
                    serde_json::from_str(custom.trim()).map_err(|_| "Enter a valid JSON number")?
                }
                _ => return Err("Choose one of the available options".into()),
            })
        } else {
            let mut selected = Vec::with_capacity(answer.selected.len());
            let mut unique = HashSet::new();

            for label in &answer.selected {
                if !unique.insert(label) {
                    return Err("An option cannot be selected more than once".into());
                }

                selected.push(
                    self.choices
                        .iter()
                        .find(|choice| choice.label == *label)
                        .ok_or("Unknown form option")?,
                );
            }

            match selected.as_slice() {
                [] => None,
                [choice] => match (&self.kind, &choice.value) {
                    (FieldKind::Array { .. }, Some(Value::String(value))) => Some(json!([value])),
                    _ => choice.value.clone(),
                },
                choices if matches!(self.kind, FieldKind::Array { .. }) => {
                    let values: Result<Vec<Value>, &str> = choices
                        .iter()
                        .map(|choice| match &choice.value {
                            Some(Value::String(value)) => Ok(json!(value)),
                            _ => Err("Blank, empty, and default choices must be selected alone"),
                        })
                        .collect();

                    Some(Value::Array(values?))
                }
                _ => return Err("Select only one option for this field".into()),
            }
        };

        match &value {
            Some(value) => self.kind.validate(value)?,
            None if self.required => return Err("This field is required".into()),
            None => {}
        }

        Ok(value)
    }
}

impl FieldKind {
    fn parse(schema: &Map<String, Value>) -> Result<(Self, Vec<FieldChoice>), String> {
        let common = ["type", "title", "description", "default"];

        let mut allowed = common.to_vec();
        let mut choices = Vec::new();

        let kind = match text(schema, "type")? {
            "string" if schema.contains_key("enum") || schema.contains_key("oneOf") => {
                allowed.extend(["enum", "enumNames", "oneOf"]);

                choices = enum_choices(schema, "oneOf")?;

                Self::Enum(choice_values(&choices))
            }
            "string" => {
                allowed.extend(["minLength", "maxLength", "format"]);

                let (minimum, maximum) = size_bounds(schema, "minLength", "maxLength")?;

                let format = optional_text(schema, "format")?
                    .map(|format| match format {
                        "email" => Ok(StringFormat::Email),
                        "uri" => Ok(StringFormat::Uri),
                        "date" => Ok(StringFormat::Date),
                        "date-time" => Ok(StringFormat::DateTime),
                        _ => Err("Unsupported string format"),
                    })
                    .transpose()?;

                if minimum.unwrap_or(0) == 0 && format.is_none() {
                    choices.push(FieldChoice {
                        label: "Use empty text".into(),
                        value: Some(json!("")),
                    });
                }

                Self::String {
                    minimum,
                    maximum,
                    format,
                }
            }
            kind @ ("number" | "integer") => {
                allowed.extend(["minimum", "maximum"]);

                let minimum = optional_number(schema, "minimum")?;
                let maximum = optional_number(schema, "maximum")?;

                if minimum.zip(maximum).is_some_and(|(min, max)| min > max) {
                    return Err("Minimum cannot exceed maximum".into());
                }

                Self::Number {
                    integer: kind == "integer",
                    minimum,
                    maximum,
                }
            }
            "boolean" => {
                choices.extend([true, false].map(|value| FieldChoice {
                    label: if value { "Yes" } else { "No" }.into(),
                    value: Some(json!(value)),
                }));

                Self::Boolean
            }
            "array" => {
                allowed.extend(["items", "minItems", "maxItems"]);

                let items = object(schema.get("items").ok_or("Missing array items")?, "items")?;

                check_keys(items, &["type", "enum", "anyOf"])?;

                if optional_text(items, "type")?.is_some_and(|kind| kind != "string") {
                    return Err("Array items must be strings".into());
                }

                if items.contains_key("enum") && !items.contains_key("type") {
                    return Err("Enum array items require type string".into());
                }

                choices = enum_choices(items, "anyOf")?;

                let values = choice_values(&choices);
                let (minimum, maximum) = size_bounds(schema, "minItems", "maxItems")?;

                if minimum.unwrap_or(0) == 0 {
                    choices.push(FieldChoice {
                        label: "Select no values".into(),
                        value: Some(json!([])),
                    });
                }

                if minimum.is_some_and(|minimum| minimum > values.len() as u64) {
                    return Err("Array minimum exceeds the number of available values".into());
                }

                Self::Array {
                    values,
                    minimum,
                    maximum,
                }
            }
            _ => return Err("Unsupported elicitation field type".into()),
        };

        check_keys(schema, &allowed)?;

        Ok((kind, choices))
    }

    fn validate(&self, value: &Value) -> Result<(), String> {
        match self {
            Self::String {
                minimum,
                maximum,
                format,
            } => {
                let value = value.as_str().ok_or("Expected text")?;

                validate_size(value.chars().count(), *minimum, *maximum, "characters")?;

                if let Some(format) = format {
                    format.validate(value)?;
                }
            }
            Self::Number {
                integer,
                minimum,
                maximum,
            } => {
                let number = value.as_f64().ok_or("Expected a number")?;

                if !number.is_finite() || (*integer && number.fract() != 0.0) {
                    return Err("Expected a finite integer".into());
                }

                if minimum.is_some_and(|minimum| number < minimum) {
                    return Err(format!(
                        "Value must be at least {}",
                        minimum.unwrap_or_default()
                    ));
                }

                if maximum.is_some_and(|maximum| number > maximum) {
                    return Err(format!(
                        "Value must be at most {}",
                        maximum.unwrap_or_default()
                    ));
                }
            }
            Self::Boolean if !value.is_boolean() => return Err("Expected a boolean".into()),
            Self::Boolean => {}
            Self::Enum(values) => {
                if !value
                    .as_str()
                    .is_some_and(|value| values.iter().any(|v| v == value))
                {
                    return Err("Value is not one of the available choices".into());
                }
            }
            Self::Array {
                values,
                minimum,
                maximum,
            } => {
                let selected = value.as_array().ok_or("Expected a list of choices")?;

                validate_size(selected.len(), *minimum, *maximum, "selected values")?;

                let mut unique = HashSet::new();

                for value in selected {
                    let value = value.as_str().ok_or("Array choices must be strings")?;

                    if !values.iter().any(|allowed| allowed == value) || !unique.insert(value) {
                        return Err("Array contains an unknown or duplicate choice".into());
                    }
                }
            }
        }

        Ok(())
    }

    fn instructions(&self) -> String {
        match self {
            Self::String {
                minimum,
                maximum,
                format,
            } => {
                let kind = match format {
                    Some(StringFormat::Email) => "Enter an email address.",
                    Some(StringFormat::Uri) => "Enter an absolute URI.",
                    Some(StringFormat::Date) => "Enter a date as YYYY-MM-DD.",
                    Some(StringFormat::DateTime) => {
                        "Enter a date-time with a UTC offset, such as 2026-01-01T12:00:00Z."
                    }
                    None => "Enter text.",
                };

                format!(
                    "{kind}{}",
                    bounds_description(*minimum, *maximum, "characters")
                )
            }
            Self::Number {
                integer,
                minimum,
                maximum,
            } => {
                let mut description = if *integer {
                    "Enter an integer."
                } else {
                    "Enter a number."
                }
                .to_owned();

                if let Some(minimum) = minimum {
                    description.push_str(&format!(" Minimum: {minimum}."));
                }

                if let Some(maximum) = maximum {
                    description.push_str(&format!(" Maximum: {maximum}."));
                }

                description
            }
            Self::Boolean => "Choose Yes or No.".into(),
            Self::Enum(_) => "Choose one value.".into(),
            Self::Array {
                minimum, maximum, ..
            } => format!(
                "Choose values. Blank, empty, and default options must be selected alone.{}",
                bounds_description(*minimum, *maximum, "values"),
            ),
        }
    }
}

impl StringFormat {
    fn validate(&self, value: &str) -> Result<(), String> {
        let valid = match self {
            Self::Email => valid_email(value),
            Self::Uri => {
                !value.chars().any(char::is_whitespace)
                    && !value.chars().any(char::is_control)
                    && Url::parse(value).is_ok()
            }
            Self::Date => {
                value.len() == 10
                    && value.bytes().enumerate().all(|(index, byte)| {
                        if matches!(index, 4 | 7) {
                            byte == b'-'
                        } else {
                            byte.is_ascii_digit()
                        }
                    })
                    && NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
            }
            Self::DateTime => {
                matches!(value.as_bytes().get(10), Some(b'T' | b't'))
                    && DateTime::parse_from_rfc3339(value).is_ok()
            }
        };

        if valid {
            Ok(())
        } else {
            Err("Value does not match the requested text format".into())
        }
    }
}

fn enum_choices(schema: &Map<String, Value>, titled_key: &str) -> Result<Vec<FieldChoice>, String> {
    let choices = match (schema.get("enum"), schema.get(titled_key)) {
        (Some(values), None) => {
            let values = strings(values, "enum")?;
            let titles = optional_strings(schema, "enumNames")?;

            if titles
                .as_ref()
                .is_some_and(|titles| titles.len() != values.len())
            {
                return Err("Enum names must match the enum value count".into());
            }

            values
                .into_iter()
                .enumerate()
                .map(|(index, value)| FieldChoice {
                    label: titles
                        .as_ref()
                        .map(|titles| titles[index].clone())
                        .unwrap_or_else(|| {
                            if value.is_empty() {
                                "Empty value".into()
                            } else {
                                value.clone()
                            }
                        }),
                    value: Some(json!(value)),
                })
                .collect()
        }
        (None, Some(values)) if !schema.contains_key("enumNames") => values
            .as_array()
            .ok_or("Titled enum must be an array")?
            .iter()
            .map(|value| {
                let value = object(value, "titled enum choice")?;

                check_keys(value, &["const", "title"])?;

                Ok(FieldChoice {
                    label: text(value, "title")?.into(),
                    value: Some(json!(text(value, "const")?)),
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        _ => return Err("Enum must define exactly one supported set of choices".into()),
    };

    let mut unique = HashSet::new();

    if choices.is_empty()
        || choices
            .iter()
            .any(|choice| !unique.insert(choice.value.as_ref().and_then(Value::as_str)))
    {
        return Err("Enum values must be nonempty and distinct".into());
    }

    Ok(choices)
}

fn choice_values(choices: &[FieldChoice]) -> Vec<String> {
    choices
        .iter()
        .filter_map(|choice| choice.value.as_ref()?.as_str().map(str::to_owned))
        .collect()
}

fn confirmation_question(question: String, label: &str) -> Question {
    Question {
        header: Some("MCP request".into()),
        question,
        multi_select: false,
        options: vec![QuestionOption {
            label: label.into(),
            description: None,
        }],
        input: QuestionInput::SelectionOnly,
    }
}

fn validate_confirmation(answer: &QuestionAnswer, label: &str) -> Result<(), String> {
    if answer.custom.is_some() || answer.selected.as_slice() != [label] {
        return Err("Select the confirmation option before submitting".into());
    }

    Ok(())
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{name} must be an object"))
}

fn text<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a str, String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{name} must be a string"))
}

fn optional_text<'a>(
    object: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<&'a str>, String> {
    object.get(name).map(|_| text(object, name)).transpose()
}

fn strings(value: &Value, name: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} must contain strings"))
        })
        .collect()
}

fn optional_strings(
    object: &Map<String, Value>,
    name: &str,
) -> Result<Option<Vec<String>>, String> {
    object
        .get(name)
        .map(|value| strings(value, name))
        .transpose()
}

fn check_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("Unsupported schema property: {key}"));
    }

    Ok(())
}

fn size_bounds(
    schema: &Map<String, Value>,
    minimum: &str,
    maximum: &str,
) -> Result<(Option<u64>, Option<u64>), String> {
    let parse = |name| {
        schema
            .get(name)
            .map(|value| {
                value
                    .as_u64()
                    .ok_or_else(|| format!("{name} must be a nonnegative integer"))
            })
            .transpose()
    };

    let bounds = (parse(minimum)?, parse(maximum)?);

    if bounds.0.zip(bounds.1).is_some_and(|(min, max)| min > max) {
        return Err("Minimum cannot exceed maximum".into());
    }

    Ok(bounds)
}

fn optional_number(schema: &Map<String, Value>, name: &str) -> Result<Option<f64>, String> {
    schema
        .get(name)
        .map(|value| {
            value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("{name} must be a finite number"))
        })
        .transpose()
}

fn validate_size(
    size: usize,
    minimum: Option<u64>,
    maximum: Option<u64>,
    unit: &str,
) -> Result<(), String> {
    if let Some(minimum) = minimum.filter(|minimum| (size as u64) < *minimum) {
        return Err(format!("Provide at least {minimum} {unit}"));
    }

    if let Some(maximum) = maximum.filter(|maximum| (size as u64) > *maximum) {
        return Err(format!("Provide at most {maximum} {unit}"));
    }

    Ok(())
}

fn bounds_description(minimum: Option<u64>, maximum: Option<u64>, unit: &str) -> String {
    let mut description = String::new();

    if let Some(minimum) = minimum {
        description.push_str(&format!(" Minimum: {minimum} {unit}."));
    }

    if let Some(maximum) = maximum {
        description.push_str(&format!(" Maximum: {maximum} {unit}."));
    }

    description
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) if value.is_empty() => "(empty text)".into(),
        Value::String(value) => value.clone(),
        Value::Array(values) if values.is_empty() => "(no values)".into(),
        Value::Array(values) => values
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Bool(value) => if *value { "Yes" } else { "No" }.into(),
        _ => value.to_string(),
    }
}

fn markdown_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '<'
                | '>'
                | '('
                | ')'
                | '#'
                | '!'
                | '|'
        ) {
            escaped.push('\\');
        }

        escaped.push(character);
    }

    escaped
}

fn valid_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };

    if local.is_empty()
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !local.chars().all(|character| {
            character.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~.".contains(character)
        })
    {
        return false;
    }

    let labels: Vec<&str> = domain.split('.').collect();

    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
}
