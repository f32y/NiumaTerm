use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::CodexProviderConfig;
use crate::chat::{
    Compaction, ContextUsageScope, ContextWindowUsage, Event, Item, ModelInfo, ScopedTokenUsage,
    SessionSummary, SkillReference, SlashCommandOutcome, ThreadSettings, TokenUsageBreakdown,
};
use crate::codex::app_server::{
    PROVIDER_API_FIELD, THREAD_LIST_LIMIT, THREAD_RESUME_RPC_ID, THREAD_START_RPC_ID, ThreadProfile,
};

pub(super) fn parse_context_window_usage(value: &Value) -> Option<ContextWindowUsage> {
    let current = parse_token_usage_breakdown(&value["last"])?;
    if current.total_tokens == 0 {
        return None;
    }

    Some(ContextWindowUsage {
        current,
        cumulative: parse_token_usage_breakdown(&value["total"]).map(|breakdown| {
            ScopedTokenUsage {
                scope: ContextUsageScope::Thread,
                breakdown,
            }
        }),
        max_tokens: value["modelContextWindow"]
            .as_u64()
            .filter(|value| *value > 0),
    })
}

pub(super) fn parse_token_usage_breakdown(value: &Value) -> Option<TokenUsageBreakdown> {
    Some(TokenUsageBreakdown {
        total_tokens: value["totalTokens"].as_u64()?,
        input_tokens: value["inputTokens"].as_u64(),
        cache_read_input_tokens: value["cachedInputTokens"].as_u64(),
        cache_write_input_tokens: value["cacheWriteInputTokens"].as_u64(),
        output_tokens: value["outputTokens"].as_u64(),
        reasoning_output_tokens: value["reasoningOutputTokens"].as_u64(),
    })
}

pub(super) fn codex_command_request(rpc_id: u64, thread_id: &str, name: &str) -> Option<Value> {
    match name {
        "compact" => Some(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "thread/compact/start",
            "params": {"threadId": thread_id},
        })),
        "review" => Some(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "review/start",
            "params": {
                "threadId": thread_id,
                "delivery": "inline",
                "target": {"type": "uncommittedChanges"},
            },
        })),
        _ => None,
    }
}

pub(super) fn skills_list_request(rpc_id: u64, force_reload: bool) -> Value {
    let params = if force_reload {
        json!({"forceReload": true})
    } else {
        json!({})
    };

    json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "method": "skills/list",
        "params": params,
    })
}

pub(super) fn codex_user_input(text: &str, skill: Option<&SkillReference>) -> Value {
    let mut input = vec![json!({"type": "text", "text": text})];

    if let Some(skill) = skill {
        input.push(json!({
            "type": "skill",
            "name": &skill.name,
            "path": &skill.path,
        }));
    }

    Value::Array(input)
}

pub(super) fn codex_command_response(name: &str, error: Option<&str>) -> SlashCommandOutcome {
    if let Some(error) = error {
        return SlashCommandOutcome::Rejected {
            message: format!("/{name} failed: {error}"),
        };
    }

    // Dedicated command RPCs acknowledge scheduling before their turn and
    // item notifications report the actual work. Treating this response as
    // completion can admit another queued command while the thread is busy.
    SlashCommandOutcome::Accepted
}

pub(super) fn delta_event(params: &Value, make: fn(String, String) -> Event) -> Vec<Event> {
    match (params["itemId"].as_str(), params["delta"].as_str()) {
        (Some(item_id), Some(delta)) => vec![make(item_id.to_string(), delta.to_string())],
        _ => Vec::new(),
    }
}

pub(super) fn add_provider_config(params: &mut Value, provider: &CodexProviderConfig) {
    let mut provider_value = json!({
        "name": provider.name.as_str(),
        "base_url": provider.base_url.as_str(),
    });
    provider_value[PROVIDER_API_FIELD] = json!("responses");
    if let Some(env_key) = provider.api_key_env.as_deref() {
        provider_value["env_key"] = json!(env_key);
    }

    let mut config = serde_json::Map::new();
    config.insert(format!("model_providers.{}", provider.id), provider_value);
    params["config"] = Value::Object(config);
}

pub(super) fn thread_start_params(profile: &ThreadProfile) -> Value {
    let mut params = json!({});
    if let Some(model) = profile.model.as_deref() {
        params["model"] = json!(model);
    }
    if let Some(provider) = profile.provider.as_ref() {
        params["modelProvider"] = json!(provider.id.as_str());
        add_provider_config(&mut params, provider);
    }
    params
}

pub(super) fn initial_thread_request(thread_id: Option<&str>, profile: &ThreadProfile) -> Value {
    if let Some(thread_id) = thread_id {
        json!({
            "jsonrpc": "2.0",
            "id": THREAD_RESUME_RPC_ID,
            "method": "thread/resume",
            "params": thread_resume_params(thread_id, profile),
        })
    } else {
        json!({
            "jsonrpc": "2.0",
            "id": THREAD_START_RPC_ID,
            "method": "thread/start",
            "params": thread_start_params(profile),
        })
    }
}

pub(super) fn thread_resume_params(thread_id: &str, profile: &ThreadProfile) -> Value {
    let mut params = json!({"threadId": thread_id});
    if let Some(model) = profile.model.as_deref() {
        params["model"] = json!(model);
    }
    if let Some(provider) = profile.provider.as_ref() {
        // With no explicit model, omitting modelProvider lets Codex restore
        // both the persisted model and provider id while this config entry
        // makes that provider resolvable in the new app-server process.
        if profile.model.is_some() {
            params["modelProvider"] = json!(provider.id.as_str());
        }
        add_provider_config(&mut params, provider);
    }
    params
}

pub(super) fn thread_list_params(profile: &ThreadProfile, cursor: Option<&str>) -> Value {
    let mut params = json!({
        "cwd": ".",
        "sortKey": "recency_at",
        "limit": THREAD_LIST_LIMIT,
    });
    if let Some(provider) = profile.provider.as_ref() {
        params["modelProviders"] = json!([provider.id.as_str()]);
    }
    if let Some(cursor) = cursor {
        params["cursor"] = json!(cursor);
    }
    params
}

pub(super) fn parse_thread_settings(result: &Value) -> ThreadSettings {
    ThreadSettings {
        model: result["model"].as_str().map(str::to_owned),
        approval: result["approvalPolicy"].as_str().map(str::to_owned),
        sandbox: result["sandbox"]["type"].as_str().map(str::to_owned),
        effort: result["reasoningEffort"].as_str().map(str::to_owned),
        tier: result["serviceTier"].as_str().map(str::to_owned),
    }
}

pub(super) fn resumed_thread_events(result: &Value, suppress_replay: bool) -> Vec<Event> {
    let mut events = Vec::new();
    if !suppress_replay {
        events.push(Event::Replay(parse_replay(&result["thread"]["turns"])));
    }
    // Resume restores the thread's persisted model/effort; Ready re-seeds the
    // pickers even when replay is suppressed for a retained transcript.
    events.push(Event::Ready(parse_thread_settings(result)));
    events
}

pub(super) fn parse_models(result: &Value, selected_model: Option<&str>) -> Vec<ModelInfo> {
    let mut models: Vec<ModelInfo> = result["data"]
        .as_array()
        .map(|data| {
            data.iter()
                .filter(|m| !m["hidden"].as_bool().unwrap_or(false))
                .filter_map(|m| {
                    let model = m["model"].as_str()?.to_string();
                    let display = m["displayName"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&model)
                        .to_string();
                    let tiers = m["serviceTiers"]
                        .as_array()
                        .map(|tiers| {
                            tiers
                                .iter()
                                .filter_map(|tier| {
                                    let id = tier["id"].as_str()?.to_string();
                                    let name = tier["name"]
                                        .as_str()
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or(&id)
                                        .to_string();
                                    Some((id, name))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let default_tier = m["defaultServiceTier"].as_str().map(str::to_owned);

                    Some(ModelInfo {
                        model,
                        display,
                        tiers,
                        default_tier,
                        efforts: Vec::new(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(model) = selected_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        && !models.iter().any(|entry| entry.model == model)
    {
        models.insert(
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

    models
}

/// One `thread/list` page as backend-neutral summaries, skipping
/// `own_thread` (the listing includes the thread this session just started).
pub(super) fn parse_thread_summaries(
    result: &Value,
    own_thread: Option<&str>,
) -> Vec<SessionSummary> {
    result["data"]
        .as_array()
        .map(|data| {
            data.iter()
                .filter_map(|thread| {
                    let id = thread["id"].as_str()?.to_string();

                    if Some(id.as_str()) == own_thread {
                        return None;
                    }

                    let title = thread["name"]
                        .as_str()
                        .or_else(|| thread["preview"].as_str())
                        .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| id.chars().take(8).collect());
                    // Backend timestamps are unix seconds; `recencyAt` advances
                    // when a turn starts, which matches "last active" better
                    // than `updatedAt` (background mutations move that).
                    let seconds = thread["recencyAt"]
                        .as_u64()
                        .or_else(|| thread["updatedAt"].as_u64())
                        .unwrap_or_default();
                    let branch = thread["gitInfo"]["branch"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);

                    Some(SessionSummary {
                        id,
                        title,
                        branch,
                        last_active: UNIX_EPOCH + Duration::from_secs(seconds),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Flatten a resumed thread's `turns[].items` into replay entries while using
/// the same typed item parser as live notifications. Keeping one parser is the
/// invariant that prevents restored tool cards from losing output or status as
/// the app-server schema evolves.
pub(super) fn parse_replay(turns: &Value) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();

    for item in turns
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|turn| turn["items"].as_array())
        .flatten()
    {
        match item["type"].as_str() {
            // Hook prompts are provider plumbing rather than transcript
            // activity. Every supported transcript item goes through the live
            // parser so dialogue, command output, diffs, and tool results
            // cannot diverge between live and restored sessions.
            Some("hookPrompt") | None => {}
            Some(_) => {
                if let Some(item) = parse_item(item) {
                    let visible = match &item {
                        Item::UserMessage { text } | Item::AgentMessage { text, .. } => {
                            text.as_deref().is_some_and(|text| !text.trim().is_empty())
                        }
                        _ => true,
                    };
                    if visible {
                        items.push(item);
                    }
                }
            }
        }
    }

    items
}

/// A user message item's `content` is an array of typed `UserInput` blocks.
pub(super) fn user_input_text(content: &Value) -> String {
    let parts: Vec<&str> = content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"].as_str() == Some("text"))
        .filter_map(|block| block["text"].as_str())
        .collect();

    parts.join("\n").trim().to_string()
}

pub(super) fn parse_item(item: &Value) -> Option<Item> {
    let id = item["id"].as_str().unwrap_or_default().to_string();
    let status = item["status"].as_str().map(str::to_owned);

    let parsed = match item["type"].as_str()? {
        "userMessage" => {
            let text = user_input_text(&item["content"]);
            Item::UserMessage {
                text: (!text.is_empty()).then_some(text),
            }
        }
        "agentMessage" => Item::AgentMessage {
            id,
            text: item["text"].as_str().map(str::to_owned),
        },
        "reasoning" => Item::Reasoning {
            id,
            summary: item["summary"].as_array().map(|summary| {
                summary
                    .iter()
                    .filter_map(|part| part.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
        },
        "commandExecution" => Item::CommandExecution {
            id,
            command: stringify_command(&item["command"]),
            aggregated_output: item["aggregatedOutput"].as_str().map(str::to_owned),
            status,
            exit_code: item["exitCode"].as_i64(),
        },
        "fileChange" => Item::FileChange {
            id,
            paths: file_change_paths(&item["changes"]),
            diff: file_change_diff(&item["changes"]),
            status,
        },
        "contextCompaction" => Item::Compaction {
            id,
            detail: Compaction::default(),
        },
        kind => Item::Other {
            id,
            kind: kind.to_string(),
            title: tool_title(item),
            output: tool_output(item),
            status,
        },
    };

    Some(parsed)
}

/// Best-effort result payload of a generic tool item. Field names vary by
/// item kind and server version; structured payloads pretty-print as JSON so
/// the transcript card can render (and highlight) them.
pub(super) fn tool_output(item: &Value) -> Option<String> {
    for key in ["output", "result", "aggregatedOutput", "content"] {
        let value = &item[key];
        if let Some(text) = value.as_str() {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        } else if value.is_object() || value.is_array() {
            return serde_json::to_string_pretty(value).ok();
        }
    }
    None
}

/// The protocol sends commands as either a shell string or an argv array.
pub(super) fn stringify_command(command: &Value) -> String {
    match command {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| p.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

pub(super) fn file_change_paths(changes: &Value) -> String {
    let paths: Vec<&str> = changes
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|change| change["path"].as_str())
                .collect()
        })
        .unwrap_or_default();

    if paths.is_empty() {
        "(unknown files)".to_string()
    } else {
        paths.join(", ")
    }
}

/// Concatenate whatever diff text the backend provides for each change. Field
/// names vary across server versions (and sit either on the change or inside
/// its `kind`); absent diffs just leave the card without expandable detail.
pub(super) fn file_change_diff(changes: &Value) -> Option<String> {
    let parts: Vec<&str> = changes
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|change| {
                    ["diff", "unified_diff", "unifiedDiff"]
                        .iter()
                        .find_map(|k| {
                            change[*k]
                                .as_str()
                                .or_else(|| change["kind"][*k].as_str())
                                .filter(|s| !s.trim().is_empty())
                        })
                })
                .collect()
        })
        .unwrap_or_default();

    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Best-effort one-line label for an arbitrary tool item: MCP calls have
/// `server` + `tool`, dynamic tools `tool`, web searches `query`.
pub(super) fn tool_title(item: &Value) -> String {
    match item["type"].as_str() {
        Some("enteredReviewMode") => return "Entered review mode".to_string(),
        Some("exitedReviewMode") => return "Exited review mode".to_string(),
        _ => {}
    }

    let tool = item["tool"].as_str();

    match (item["server"].as_str(), tool) {
        (Some(server), Some(tool)) => format!("{server}/{tool}"),
        (None, Some(tool)) => tool.to_string(),
        _ => item["query"].as_str().unwrap_or_default().to_string(),
    }
}
