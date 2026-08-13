use nmt_i18n::i18n;

use crate::agent_pane::*;

pub(in crate::agent_pane) fn should_show_jump_to_latest(
    is_following_tail: bool,
    is_scrolled_to_end: Option<bool>,
    max_scroll_offset: Pixels,
) -> bool {
    if is_following_tail {
        return false;
    }

    match is_scrolled_to_end {
        Some(at_end) => !at_end,
        // A short bottom-aligned list can leave tail-follow after an upward
        // wheel gesture even though it has no scroll range. Unknown-height
        // scrollable lists still fall back to the established follow state.
        None => max_scroll_offset > px(0.),
    }
}

/// The live turn-duration and generated-token label.
pub(in crate::agent_pane) fn working_label(started: Instant, output_tokens: Option<u64>) -> String {
    working_status_label(started.elapsed().as_secs(), output_tokens)
}

pub(super) fn working_status_label(seconds: u64, output_tokens: Option<u64>) -> String {
    timed_token_label(i18n("agent-transcript-working"), seconds, output_tokens)
}

pub(super) fn worked_status_label(seconds: u64, output_tokens: Option<u64>) -> String {
    timed_token_label(i18n("agent-transcript-worked"), seconds, output_tokens)
}

pub(super) fn interrupted_status_label(output_tokens: Option<u64>) -> String {
    match output_tokens {
        Some(tokens) => i18n("agent-transcript-interrupted-tokens")
            .replace("{tokens}", &compact_token_count(tokens)),
        None => i18n("agent-transcript-interrupted").to_string(),
    }
}

pub(super) fn timed_token_label(verb: &str, seconds: u64, output_tokens: Option<u64>) -> String {
    let duration = i18n("agent-transcript-timed-status")
        .replace("{verb}", verb)
        .replace("{duration}", &elapsed_label(seconds));
    match output_tokens {
        Some(tokens) => i18n("agent-transcript-status-tokens")
            .replace("{status}", &duration)
            .replace("{tokens}", &compact_token_count(tokens)),
        None => duration,
    }
}

/// Elapsed time broken into nonzero units so sparse durations stay compact;
/// an entirely empty duration still renders as `0 s` instead of a blank label.
pub(super) fn elapsed_label(total_seconds: u64) -> String {
    let units = [
        (
            total_seconds / 86_400,
            "agent-duration-day",
            "agent-duration-days",
        ),
        (
            (total_seconds % 86_400) / 3_600,
            "agent-duration-hour",
            "agent-duration-hours",
        ),
        (
            (total_seconds % 3_600) / 60,
            "agent-duration-minute",
            "agent-duration-minutes",
        ),
    ];
    let seconds = total_seconds % 60;

    let mut parts = units
        .into_iter()
        .filter(|(value, _, _)| *value > 0)
        .map(|(value, singular, plural)| {
            i18n(if value == 1 { singular } else { plural }).replace("{count}", &value.to_string())
        })
        .collect::<Vec<_>>();

    if seconds > 0 || parts.is_empty() {
        parts.push(i18n("agent-duration-seconds").replace("{count}", &seconds.to_string()));
    }

    parts.join(" ")
}

/// Work-log rows: the single-line tool/thinking entries that participate in
/// "+N tool calls" run-collapsing. Conversation text never does.
pub(in crate::agent_pane) fn is_work_row(item: &SessionItem) -> bool {
    matches!(
        item,
        SessionItem::CommandExecution { .. }
            | SessionItem::FileChange { .. }
            | SessionItem::Other { .. }
            | SessionItem::Reasoning { .. }
    )
}

/// Entries with nothing to show (yet): an agent bubble before its first delta,
/// or a reasoning item that never streamed a summary. They render no row and
/// are transparent to work-run grouping, so an invisible entry can't split a
/// run of tool calls into two summary lines.
pub(in crate::agent_pane) fn hidden(item: &SessionItem) -> bool {
    match item {
        SessionItem::UserMessage { text }
        | SessionItem::AgentMessage { text, .. }
        | SessionItem::Reasoning { summary: text, .. } => {
            text.as_deref().is_none_or(|text| text.trim().is_empty())
        }
        _ => false,
    }
}

/// Collapsed head of an oversized user prompt, cut at a line boundary when
/// possible, or `None` when the whole prompt fits under the caps. The character
/// cap bounds visual wrapping for giant single-line pastes; a byte cap alone
/// can still produce dozens of wrapped lines in a narrow bubble.
pub(in crate::agent_pane) fn truncated_user_prompt(text: &str) -> Option<&str> {
    const MAX_SOURCE_LINES: usize = 3;
    const MAX_CHARS: usize = 512;

    let mut chars = 0;
    let mut completed_lines = 0;
    for (index, ch) in text.char_indices() {
        if chars == MAX_CHARS {
            return Some(&text[..index]);
        }
        chars += 1;

        if ch == '\n' {
            completed_lines += 1;
            let end = index + ch.len_utf8();
            if completed_lines == MAX_SOURCE_LINES && end < text.len() {
                return Some(&text[..end]);
            }
        }
    }

    None
}

/// Wrap tool output in a Markdown fence with an explicit language tag. The
/// fence grows past any backtick run in the body, so raw output can never
/// terminate the syntax-highlighted block early.
pub(in crate::agent_pane) fn fenced_code_block_as(output: &str, lang: &str) -> String {
    let mut fence = String::from("```");
    while output.contains(fence.as_str()) {
        fence.push('`');
    }
    format!("{fence}{lang}\n{output}\n{fence}")
}

/// First-bytes sniff covering the two formats tool output actually produces
/// in bulk (diffs and JSON payloads); anything else renders as an unhighlighted
/// code block. Extend per-tool only if more grammars earn their keep.
pub(in crate::agent_pane) fn detect_output_language(output: &str) -> &'static str {
    let trimmed = output.trim_start();
    if trimmed.starts_with("diff --git") || trimmed.starts_with("@@ ") {
        "diff"
    } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
        "json"
    } else {
        ""
    }
}

/// Claude's Read tool returns cat -n style lines ("   12→text"); strip the
/// gutter so the source underneath highlights as its own language. Any line
/// without the gutter leaves the text untouched (format drift safety).
pub(in crate::agent_pane) fn strip_read_gutter(output: &str) -> Option<String> {
    let mut body = String::with_capacity(output.len());

    for line in output.lines() {
        let (gutter, text) = line.split_once('→')?;
        let number = gutter.trim_start();
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        body.push_str(text);
        body.push('\n');
    }

    (!body.is_empty()).then_some(body)
}

/// Fence-language tag for a file path. The highlight registry accepts file
/// extensions as language aliases (rs, py, yml, …) and resolves unknown ones
/// to plain, so the extension itself is the tag.
pub(in crate::agent_pane) fn file_extension_lang(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub(in crate::agent_pane) fn command_execution_heading(purpose: Option<&str>) -> &str {
    purpose
        .filter(|purpose| !purpose.trim().is_empty())
        .unwrap_or_else(|| i18n("agent-transcript-run-command"))
}

pub(in crate::agent_pane) fn command_execution_detail(
    command: &str,
    aggregated_output: Option<&str>,
) -> String {
    let mut detail = String::with_capacity(
        command.len() + aggregated_output.map_or(0, str::len) + "$ \n\n".len(),
    );
    detail.push_str("$ ");
    detail.push_str(command);

    if let Some(output) = aggregated_output.filter(|output| !output.is_empty()) {
        detail.push_str("\n\n");
        detail.push_str(output);
    }

    detail
}

/// Full text of an entry for the right-click Copy action — the whole message,
/// independent of any partial selection or truncated preview.
pub(in crate::agent_pane) fn entry_copy_text(item: &SessionItem) -> String {
    match item {
        SessionItem::UserMessage { text }
        | SessionItem::AgentMessage { text, .. }
        | SessionItem::Reasoning { summary: text, .. } => text.clone().unwrap_or_default(),
        SessionItem::Error { text } => text.clone(),
        SessionItem::CommandExecution {
            command,
            aggregated_output,
            ..
        } => command_execution_detail(command, aggregated_output.as_deref()),
        SessionItem::FileChange {
            paths,
            diff,
            status,
            ..
        } => match diff {
            Some(diff) => i18n("agent-transcript-file-edit-detail")
                .replace("{paths}", paths)
                .replace("{status}", status.as_deref().unwrap_or("inProgress"))
                .replace("{diff}", diff),
            None => i18n("agent-transcript-file-edit")
                .replace("{paths}", paths)
                .replace("{status}", status.as_deref().unwrap_or("inProgress")),
        },
        SessionItem::Other {
            kind,
            title,
            output,
            status,
            ..
        } => match output {
            Some(output) => format!(
                "{kind} {title} — {}\n{output}",
                status.as_deref().unwrap_or("inProgress")
            ),
            None => format!(
                "{kind} {title} — {}",
                status.as_deref().unwrap_or("inProgress")
            ),
        },
        SessionItem::Compaction { detail, .. } => {
            let head = format!(
                "{}\n{}",
                compaction_label(detail),
                compaction_accounting(detail).join(" · ")
            );

            match &detail.summary {
                Some(summary) => format!("{head}\n\n{summary}"),
                None => head,
            }
        }
    }
}

/// Heading of a compaction row. An unprompted compaction is named as such
/// because it explains a context-gauge jump the user did not ask for.
pub(in crate::agent_pane) fn compaction_label(detail: &Compaction) -> &'static str {
    match detail.trigger {
        Some(CompactionTrigger::Automatic) => i18n("agent-transcript-context-auto-compacted"),
        Some(CompactionTrigger::Manual) | None => i18n("agent-transcript-context-compacted"),
    }
}

pub(in crate::agent_pane) fn compaction_row_is_expandable(kind: AgentKind) -> bool {
    kind == AgentKind::Claude
}

pub(in crate::agent_pane) fn compaction_trigger_label(trigger: CompactionTrigger) -> &'static str {
    match trigger {
        CompactionTrigger::Automatic => i18n("agent-transcript-trigger-automatic"),
        CompactionTrigger::Manual => i18n("agent-transcript-trigger-manual"),
    }
}

/// Token and message accounting of a compaction, as display-ready fragments.
/// Only what the backend actually reported appears, so a partially described
/// compaction shows fewer fragments instead of zeros.
pub(in crate::agent_pane) fn compaction_accounting(detail: &Compaction) -> Vec<String> {
    let mut parts = Vec::new();

    match (detail.pre_tokens, detail.post_tokens) {
        (Some(pre), Some(post)) => parts.push(format!(
            "{} → {}",
            compact_token_count(pre),
            compact_token_count(post)
        )),
        (Some(pre), None) => parts.push(
            i18n("agent-transcript-compaction-from").replace("{tokens}", &compact_token_count(pre)),
        ),
        (None, Some(post)) => parts.push(
            i18n("agent-transcript-compaction-to").replace("{tokens}", &compact_token_count(post)),
        ),
        (None, None) => {}
    }

    if let Some(pre) = detail.pre_tokens
        && let Some(post) = detail.post_tokens
        && let Some(freed) = pre.checked_sub(post).filter(|freed| *freed > 0)
    {
        parts.push(
            i18n("agent-transcript-compaction-freed")
                .replace("{tokens}", &compact_token_count(freed)),
        );
    }

    if let Some(messages) = detail.messages_summarized {
        parts.push(
            i18n("agent-transcript-compaction-messages").replace("{count}", &messages.to_string()),
        );
    }

    if let Some(trigger) = detail.trigger {
        parts.push(compaction_trigger_label(trigger).to_string());
    }

    parts
}

/// Compact "how long ago" label for a history row ("now", "5m", "3h", "2d").
pub(in crate::agent_pane) fn relative_time(at: SystemTime) -> String {
    let seconds = at.elapsed().map(|d| d.as_secs()).unwrap_or(0);

    match seconds {
        0..60 => i18n("agent-history-now").to_string(),
        60..3600 => i18n("agent-history-minutes").replace("{count}", &(seconds / 60).to_string()),
        3600..86400 => {
            i18n("agent-history-hours").replace("{count}", &(seconds / 3600).to_string())
        }
        _ => i18n("agent-history-days").replace("{count}", &(seconds / 86400).to_string()),
    }
}

pub(in crate::agent_pane) fn compact_token_count(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=9_999 => format!("{:.1}k", tokens as f64 / 1_000.0).replace(".0k", "k"),
        10_000..=999_999 => format!("{}k", tokens / 1_000),
        _ => format!("{:.1}m", tokens as f64 / 1_000_000.0).replace(".0m", "m"),
    }
}

/// Icon mirroring the current permission/approval choice (t3code's runtime
/// mode iconography): closed lock = prompts on, pen = edits auto-approved,
/// pencil-ruler = plan mode, open lock = no prompts. Covers both Claude's
/// permission modes and Codex's approval policies.
pub(in crate::agent_pane) fn permission_icon(value: Option<&str>) -> IconName {
    match value {
        Some("acceptEdits") => IconName::PenLine,
        Some("plan") => IconName::PencilRuler,
        Some("bypassPermissions") | Some("never") => IconName::LockOpen,
        _ => IconName::Lock,
    }
}
