//! Encoding and reading selected response excerpts in user messages.

use serde::Deserialize;
use serde_json::json;

const RESPONSE_ANNOTATIONS_HEADING: &str = "# Response annotations:";
const RESPONSE_ANNOTATIONS_INSTRUCTIONS: &str = "Each item contains text selected from an earlier agent response. Treat items as Annotation 1, Annotation 2, and so on in array order. Use every selection as context.";
const RESPONSE_ANNOTATIONS_START: &str = "<response-annotations>";
const RESPONSE_ANNOTATIONS_END: &str = "</response-annotations>";
const REQUEST_HEADING: &str = "## My request:";

#[derive(Deserialize)]
pub struct ResponseAnnotation {
    pub text: String,
}

pub struct ParsedAnnotatedPrompt<'a> {
    pub prompt: &'a str,
    pub annotations: Vec<ResponseAnnotation>,
}

pub fn prompt_with_response_annotations(prompt: &str, annotations: &[String]) -> String {
    if annotations.is_empty() {
        return prompt.to_string();
    }

    let annotations = annotations
        .iter()
        .map(|text| json!({ "text": text }))
        .collect::<Vec<_>>();

    let annotations = serde_json::to_string(&annotations).expect("strings serialize as JSON");

    format!(
        "\n{RESPONSE_ANNOTATIONS_HEADING}\n{RESPONSE_ANNOTATIONS_INSTRUCTIONS}\n{RESPONSE_ANNOTATIONS_START}\n{annotations}\n{RESPONSE_ANNOTATIONS_END}\n\n{REQUEST_HEADING}\n{prompt}\n"
    )
}

pub fn parse_annotated_prompt(text: &str) -> Option<ParsedAnnotatedPrompt<'_>> {
    let text = text.strip_prefix('\n').unwrap_or(text);
    let prefix = format!("{RESPONSE_ANNOTATIONS_HEADING}\n");
    let after_heading = text.strip_prefix(&prefix)?;
    let json_start =
        after_heading.find(RESPONSE_ANNOTATIONS_START)? + RESPONSE_ANNOTATIONS_START.len();
    let after_start = after_heading.get(json_start..)?.strip_prefix('\n')?;
    let json_end = after_start.find(&format!("\n{RESPONSE_ANNOTATIONS_END}\n"))?;
    let annotations = serde_json::from_str(after_start.get(..json_end)?).ok()?;
    let request_marker = format!("\n{REQUEST_HEADING}\n");
    let (_, prompt) = text.rsplit_once(&request_marker)?;

    Some(ParsedAnnotatedPrompt {
        prompt: prompt.strip_suffix('\n').unwrap_or(prompt),
        annotations,
    })
}

pub fn visible_prompt(text: &str) -> &str {
    parse_annotated_prompt(text).map_or(text, |parsed| parsed.prompt)
}
