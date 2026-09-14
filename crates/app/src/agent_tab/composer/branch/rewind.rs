pub(crate) use nmt_agent::session::branch::RewindAction;

use chrono::Local;
use rust_i18n::t;
use std::borrow::Cow;

pub(crate) fn rewind_prompt_label(prompt: &str) -> String {
    let line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(Cow::Borrowed)
        .unwrap_or_else(|| t!("agent-rewind-untitled-prompt"));

    let line = line.trim();

    let mut label = line.chars().take(72).collect::<String>();

    if line.chars().count() > 72 {
        label.push('…');
    }

    label
}

pub(crate) fn rewind_timestamp(timestamp: Option<&str>) -> Option<String> {
    let timestamp = timestamp?;

    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|time| {
            time.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .ok()
        .or_else(|| Some(timestamp.to_string()))
}
