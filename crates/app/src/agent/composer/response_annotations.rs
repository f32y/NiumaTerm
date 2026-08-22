use gpui::{Context, Window};
use gpui_component::WindowExt as _;
use serde::Deserialize;
use serde_json::json;

use crate::agent::AgentPane;

const RESPONSE_ANNOTATIONS_HEADING: &str = "# Response annotations:";
const RESPONSE_ANNOTATIONS_INSTRUCTIONS: &str = "Each item contains text selected from an earlier agent response. Treat items as Annotation 1, Annotation 2, and so on in array order. Use every selection as context.";
const RESPONSE_ANNOTATIONS_START: &str = "<response-annotations>";
const RESPONSE_ANNOTATIONS_END: &str = "</response-annotations>";
const REQUEST_HEADING: &str = "## My request:";

#[derive(Deserialize)]
pub(in crate::agent) struct ResponseAnnotation {
    pub(in crate::agent) text: String,
}

pub(in crate::agent) struct ParsedAnnotatedPrompt<'a> {
    pub(in crate::agent) prompt: &'a str,
    pub(in crate::agent) annotations: Vec<ResponseAnnotation>,
}

pub(in crate::agent) fn prompt_with_response_annotations(
    prompt: &str,
    annotations: &[String],
) -> String {
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

pub(in crate::agent) fn parse_annotated_prompt(text: &str) -> Option<ParsedAnnotatedPrompt<'_>> {
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

pub(in crate::agent) fn visible_prompt(text: &str) -> &str {
    parse_annotated_prompt(text).map_or(text, |parsed| parsed.prompt)
}

pub(in crate::agent) fn annotation_count_label(count: usize) -> String {
    let key = if count == 1 {
        "agent-composer-annotation-count-one"
    } else {
        "agent-composer-annotation-count-other"
    };
    nmt_i18n::i18n(key).replace("{count}", &count.to_string())
}

impl AgentPane {
    pub(in crate::agent) fn add_response_annotation(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }

        self.response_annotations.push(text);
        window.clear_text_selection(cx);
        self.focus(window, cx);
        cx.notify();
    }

    pub(in crate::agent) fn clear_response_annotations(&mut self, cx: &mut Context<Self>) {
        if !self.response_annotations.is_empty() {
            self.response_annotations.clear();
            cx.notify();
        }
    }
}
