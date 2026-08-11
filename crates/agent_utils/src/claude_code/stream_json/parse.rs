use std::collections::HashSet;
use std::iter::once;

use serde_json::Value;

use super::super::tool_items::tool_title;
use crate::chat::{
    ContextUsageScope, ContextWindowUsage, Event, ModelInfo, ScopedTokenUsage,
    SlashCommandArguments, SlashCommandInfo, SlashCommandRunPolicy, SlashCommandSource,
    TokenUsageBreakdown,
};

pub(super) fn parse_claude_usage(usage: &Value) -> Option<TokenUsageBreakdown> {
    let direct_input = usage["input_tokens"].as_u64();
    let cache_write_input_tokens = usage["cache_creation_input_tokens"].as_u64();
    let cache_read_input_tokens = usage["cache_read_input_tokens"].as_u64();
    let output_tokens = usage["output_tokens"].as_u64();
    let input_tokens = [
        direct_input,
        cache_write_input_tokens,
        cache_read_input_tokens,
    ]
    .into_iter()
    .flatten()
    .fold(0_u64, u64::saturating_add);
    let total_tokens = input_tokens.saturating_add(output_tokens.unwrap_or(0));

    (total_tokens > 0).then_some(TokenUsageBreakdown {
        total_tokens,
        input_tokens: (direct_input.is_some()
            || cache_write_input_tokens.is_some()
            || cache_read_input_tokens.is_some())
        .then_some(input_tokens),
        cache_read_input_tokens,
        cache_write_input_tokens,
        output_tokens,
        reasoning_output_tokens: None,
    })
}

pub(super) fn update_claude_output(usage: &mut Option<TokenUsageBreakdown>, output_tokens: u64) {
    let Some(usage) = usage else {
        return;
    };
    usage.output_tokens = Some(output_tokens);
    usage.total_tokens = usage
        .input_tokens
        .unwrap_or(0)
        .saturating_add(output_tokens);
}

/// Translate a `system/status` message into compaction progress events.
///
/// The subtype multiplexes unrelated notifications — a per-request `requesting`
/// marker, permission-mode echoes, and compaction — so each transition is
/// recognized by its own field rather than by `status` alone. `compact_result`
/// appears only on a compaction's final message, and `requesting` also fires for
/// the summarization call that compaction itself makes, which would otherwise
/// look like the end of it. `compacting` is re-announced roughly every 30
/// seconds while a long compaction runs, so `active` suppresses the repeats.
pub(super) fn compaction_progress(active: &mut bool, message: &Value) -> Vec<Event> {
    if let Some(result) = message["compact_result"].as_str() {
        *active = false;

        return vec![Event::CompactionFinished {
            error: (result != "success").then(|| {
                message["compact_error"]
                    .as_str()
                    .unwrap_or("Compacting the conversation failed.")
                    .to_string()
            }),
        }];
    }

    if message["status"].as_str() == Some("compacting") && !*active {
        *active = true;

        return vec![Event::CompactionStarted];
    }

    Vec::new()
}

pub(super) fn claude_context_window(model_usage: &Value) -> Option<u64> {
    model_usage
        .as_object()?
        .values()
        .filter_map(|usage| {
            usage["contextWindow"]
                .as_u64()
                .or_else(|| usage["context_window"].as_u64())
                .filter(|value| *value > 0)
        })
        .max()
}

pub(super) fn slash_command_text(name: &str, arguments: &str) -> String {
    let name = name.trim().trim_start_matches('/');
    let arguments = arguments.trim();

    if arguments.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} {arguments}")
    }
}

pub(super) fn ui_owns_slash_command(name: &str) -> bool {
    let name = name.trim().trim_start_matches('/');
    name.eq_ignore_ascii_case("resume") || name.eq_ignore_ascii_case("rewind")
}

pub(super) fn claude_result_error(message: &Value) -> Option<String> {
    if !message["is_error"].as_bool().unwrap_or(false)
        && message["subtype"].as_str() == Some("success")
    {
        return None;
    }

    // Startup failures (e.g. a `--resume` id whose transcript is gone) put
    // their reason in `errors`, not `result`.
    Some(
        message["result"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| message["errors"][0].as_str().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| message["subtype"].as_str().unwrap_or("turn failed"))
            .to_string(),
    )
}

/// Prefer the current structured initialize field while accepting the older
/// control-response spelling. The boolean tells first-turn handling whether
/// a later string-only catalog is allowed to replace this result.
pub(super) fn initialize_command_catalog(
    response: &Value,
) -> Option<(Vec<SlashCommandInfo>, bool)> {
    if !response["commands"].is_null() {
        Some((parse_slash_commands(&response["commands"]), true))
    } else if !response["slash_commands"].is_null() {
        Some((parse_slash_commands(&response["slash_commands"]), false))
    } else {
        None
    }
}

pub(super) fn legacy_command_catalog(
    structured_commands_published: bool,
    commands: &Value,
) -> Option<Vec<SlashCommandInfo>> {
    (!structured_commands_published).then(|| parse_slash_commands(commands))
}

/// Claude versions have emitted both string entries and richer objects. The
/// parser accepts both and expands aliases while enforcing the single-token
/// names the composer can address. A new event is a complete replacement.
pub(super) fn parse_slash_commands(commands: &Value) -> Vec<SlashCommandInfo> {
    let mut seen = HashSet::new();
    let mut parsed = Vec::new();

    for entry in commands.as_array().into_iter().flatten() {
        let Some(raw_name) = entry
            .as_str()
            .or_else(|| entry["name"].as_str())
            .or_else(|| entry["command"].as_str())
        else {
            continue;
        };
        let canonical = raw_name.trim().trim_start_matches('/').to_ascii_lowercase();
        let argument_hint = entry["argumentHint"]
            .as_str()
            .or_else(|| entry["argument_hint"].as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let description = entry["description"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Run Claude's /{canonical} command"));
        let aliases = entry["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);

        for raw_name in once(raw_name).chain(aliases) {
            let name = raw_name.trim().trim_start_matches('/').to_ascii_lowercase();

            if name.is_empty()
                || name.chars().any(char::is_whitespace)
                || !seen.insert(name.clone())
            {
                continue;
            }

            parsed.push(SlashCommandInfo {
                name,
                description: description.clone(),
                // The catalog advertises a hint when it has one and says
                // nothing otherwise; it never states that a command refuses
                // arguments. Skills and prompt commands routinely carry no
                // hint yet take free-form input, so treating a missing hint as
                // a refusal rejects them before the CLI ever sees them.
                arguments: SlashCommandArguments::Freeform,
                argument_hint: argument_hint.clone(),
                source: SlashCommandSource::Provider,
                run_policy: SlashCommandRunPolicy::QueueUntilIdle,
            });
        }
    }

    parsed
}

/// Human-readable summary of a `can_use_tool` request for the approval card.
pub(super) fn approval_description(tool_name: &str, input: &Value) -> String {
    match tool_name {
        "Bash" => format!(
            "Run command: `{}`",
            input["command"].as_str().unwrap_or_default()
        ),
        "Edit" | "Write" | "NotebookEdit" => format!(
            "Edit file: {}",
            input["file_path"].as_str().unwrap_or("(unknown file)")
        ),
        "ExitPlanMode" => format!(
            "Approve Claude's plan:\n\n{}",
            input["plan"].as_str().unwrap_or_default()
        ),
        _ => {
            let detail = tool_title(input);

            if detail.is_empty() {
                tool_name.to_string()
            } else {
                format!("{tool_name}: {detail}")
            }
        }
    }
}

/// The model catalog from the initialize response: `value` is the protocol name
/// (`"default"`, `"opus[1m]"`, …), `displayName` the menu label. Claude has
/// no per-model service tiers, but each entry lists its reasoning-effort
/// levels in `supportedEffortLevels` (absent on models without effort, e.g.
/// Haiku).
pub(super) fn parse_models(models: &Value, selected_model: Option<&str>) -> Vec<ModelInfo> {
    let mut parsed: Vec<ModelInfo> = models
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    let model = entry["value"].as_str()?.to_string();
                    let display = entry["displayName"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&model)
                        .to_string();
                    let efforts = entry["supportedEffortLevels"]
                        .as_array()
                        .map(|levels| {
                            levels
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();

                    Some(ModelInfo {
                        model,
                        display,
                        tiers: Vec::new(),
                        default_tier: None,
                        efforts,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(model) = selected_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        && !parsed.iter().any(|entry| entry.model == model)
    {
        parsed.insert(
            0,
            ModelInfo {
                model: model.to_string(),
                display: model.to_string(),
                tiers: Vec::new(),
                default_tier: None,
                efforts: Vec::new(),
            },
        );
    }

    parsed
}

pub(super) fn context_window_usage(
    current: Option<TokenUsageBreakdown>,
    last_turn: Option<TokenUsageBreakdown>,
    max_tokens: Option<u64>,
) -> Option<ContextWindowUsage> {
    let current = current.filter(|usage| usage.total_tokens > 0)?;

    Some(ContextWindowUsage {
        current,
        cumulative: last_turn.map(|breakdown| ScopedTokenUsage {
            scope: ContextUsageScope::LastTurn,
            breakdown,
        }),
        max_tokens,
    })
}
