//! Turning host frames into the backend-neutral chat vocabulary.
//!
//! The host publishes an already-normalized session event stream, so this maps
//! rather than interprets: there is no vendor stream to reassemble, and
//! anything unrecognized becomes nothing at all instead of an error.

use std::collections::HashMap;

use serde_json::Value;

use crate::chat::{
    Compaction, CompactionTrigger, Event, Item, Question, QuestionOption, TurnActivity,
};

/// The status vocabulary the transcript renders: anything else reads as still
/// running, and `failed` is what turns a row red.
const IN_PROGRESS: &str = "inProgress";
const COMPLETED: &str = "completed";
const FAILED: &str = "failed";

/// An approval the harness is blocked on. The turn does not continue until it
/// is answered, so a client that recognizes the frame and then does nothing
/// leaves the agent waiting with no way for the user to see why.
///
/// `rpc_id` correlates the answer and `approval_id` is the harness's own audit
/// identity; both have to travel back on the reply, and neither is derivable
/// from the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovalRequest {
    pub(crate) rpc_id: String,
    pub(crate) approval_id: String,
    pub(crate) description: String,
}

/// Recognize an answerable approval frame addressed to this session.
///
/// This is separate from [`map_frame`] because answering is a side effect the
/// session has to own: the identities below are needed later, when the user
/// decides, and nothing in the transcript vocabulary carries them.
pub(crate) fn approval_request(frame: &Value, session_id: &str) -> Option<ApprovalRequest> {
    let payload = &frame["payload"];
    if payload["type"] != "approval/requested" || payload["sessionId"].as_str() != Some(session_id)
    {
        return None;
    }

    let tool = payload["toolName"].as_str().unwrap_or("a tool");
    let reason = payload["reason"].as_str().unwrap_or_default();
    // The reason is the asker's own sentence and already reads as an
    // explanation; the tool name is prepended because the reason does not
    // always name what is about to run.
    let description = if reason.is_empty() {
        format!("{tool}\n\nThe agent is asking to run this.")
    } else {
        format!("{tool}\n\n{reason}")
    };

    Some(ApprovalRequest {
        rpc_id: frame["rpcId"].as_str()?.to_string(),
        approval_id: payload["approvalId"].as_str()?.to_string(),
        description,
    })
}

/// A batch of questions the harness is blocked on.
///
/// `ids` is kept because the harness matches an answer positionally against the
/// question ids it asked, while the transcript vocabulary carries only the
/// question text. Answering therefore needs the ask order preserved, which is
/// also why this holds the whole batch rather than one question at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuestionRequest {
    pub(crate) rpc_id: String,
    pub(crate) ids: Vec<String>,
}

/// Recognize an answerable question frame addressed to this session.
///
/// Separate from [`map_frame`] for the same reason [`approval_request`] is: the
/// identities an answer has to carry outlive the event that raised the card.
pub(crate) fn question_request(
    frame: &Value,
    session_id: &str,
) -> Option<(QuestionRequest, Vec<Question>)> {
    let payload = &frame["payload"];
    if payload["type"] != "question/requested" || payload["sessionId"].as_str() != Some(session_id)
    {
        return None;
    }

    let asked = payload["questions"].as_array()?;
    let mut ids = Vec::with_capacity(asked.len());
    let mut questions = Vec::with_capacity(asked.len());

    for item in asked {
        // A question with no id cannot be answered — the harness rejects the
        // whole batch when one answer fails to name the question it settles —
        // so the card is not raised at all rather than raised unanswerable.
        let id = item["id"].as_str()?;
        ids.push(id.to_string());
        questions.push(Question {
            header: item["header"].as_str().map(str::to_string),
            // `detail` is supporting text the harness deliberately keeps out of
            // the option labels; folding it into the question text is what puts
            // it in front of the user, because the card has no separate slot.
            question: match item["detail"].as_str() {
                Some(detail) if !detail.is_empty() => {
                    format!(
                        "{}\n\n{detail}",
                        item["question"].as_str().unwrap_or_default()
                    )
                }
                _ => item["question"].as_str().unwrap_or_default().to_string(),
            },
            multi_select: item["multiSelect"].as_bool().unwrap_or(false),
            options: item["options"]
                .as_array()
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| {
                            Some(QuestionOption {
                                label: option["label"].as_str()?.to_string(),
                                description: option["description"].as_str().map(str::to_string),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        });
    }

    Some((
        QuestionRequest {
            rpc_id: frame["rpcId"].as_str()?.to_string(),
            ids,
        },
        questions,
    ))
}

/// Frames belonging to a session this client does not own, or carrying a type
/// this build does not know, produce no events. Both are normal: the mux stream
/// is aggregated across every attached session, and the harness adds event
/// types between releases.
pub(crate) fn map_frame(frame: &Value, session_id: &str, tools: &mut ToolTracker) -> Vec<Event> {
    let payload = &frame["payload"];

    match payload["type"].as_str() {
        Some("session/event") => {
            if payload["sessionId"].as_str() != Some(session_id) {
                return Vec::new();
            }
            // The host computes the render card and attaches it to the frame,
            // not to the logged event, so it travels separately.
            map_session_event(&payload["event"], &payload["view"], tools)
        }
        Some("host/agent-error") if payload["sessionId"].as_str() == Some(session_id) => {
            match payload["message"].as_str() {
                Some(message) => vec![Event::ItemStarted(Item::Error {
                    text: message.to_string(),
                })],
                None => Vec::new(),
            }
        }
        // Whoever answered, the card comes down: the same approval can be
        // resolved by another client, or by the turn ending under it.
        Some("approval/resolved") if payload["sessionId"].as_str() == Some(session_id) => {
            vec![Event::ApprovalResolved]
        }
        Some("question/resolved") if payload["sessionId"].as_str() == Some(session_id) => {
            vec![Event::QuestionsResolved]
        }
        Some("stream/error") => match payload["error"]["message"].as_str() {
            Some(message) => vec![Event::ItemStarted(Item::Error {
                text: message.to_string(),
            })],
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The tool calls a session has started but not yet seen a result for.
///
/// The result event names only the call it answers, so what kind of transcript
/// row it belongs to — and the command or paths that row already shows — is
/// knowable only from the call that opened it.
#[derive(Default)]
pub(crate) struct ToolTracker {
    started: HashMap<String, Item>,
}

/// Which transcript row a tool call becomes, decided by the render card the
/// host computed rather than by the tool's name.
///
/// The host states how a call should read, so a tool this build has never heard
/// of still lands in the right row; keying on names would need a table updated
/// on every harness release, and would silently mis-render until it was.
fn started_tool_item(call: &Value, view: &Value) -> Item {
    let id = call["callId"].as_str().unwrap_or_default().to_string();
    let name = call["name"].as_str().unwrap_or("tool").to_string();
    let card = view["card"].as_str().unwrap_or("generic");
    let title = view["title"].as_str().unwrap_or(&name).to_string();

    match card {
        "terminal" => Item::CommandExecution {
            id,
            // A terminal card's title is the command line itself.
            command: title,
            purpose: view["description"].as_str().map(str::to_string),
            aggregated_output: None,
            status: Some(IN_PROGRESS.to_string()),
            exit_code: None,
        },
        "diff" => Item::FileChange {
            id,
            paths: diff_paths(&view["diffs"]),
            diff: render_diffs(&view["diffs"]),
            status: Some(IN_PROGRESS.to_string()),
        },
        _ => Item::Other {
            id,
            kind: name,
            title,
            output: None,
            status: Some(IN_PROGRESS.to_string()),
        },
    }
}

/// The completed form of a row, built from the row that opened it so the
/// identity and the fields already on screen survive the update.
///
/// A failed call carries no result view at all — the presenter has nothing to
/// format — so the model-facing text is the fallback rather than an edge case.
fn completed_tool_item(started: &Item, view: &Value, message: &Value, failed: bool) -> Item {
    let status = Some(if failed { FAILED } else { COMPLETED }.to_string());

    match started {
        Item::CommandExecution { id, command, .. } => Item::CommandExecution {
            id: id.clone(),
            command: command.clone(),
            purpose: None,
            aggregated_output: view["output"]
                .as_str()
                .map(str::to_string)
                .or_else(|| result_text(message)),
            status,
            exit_code: view["exitCode"].as_i64(),
        },
        Item::FileChange { id, paths, .. } => Item::FileChange {
            id: id.clone(),
            paths: paths.clone(),
            // The result diff carries surrounding context the arguments did
            // not, so it replaces the call-time one when present.
            diff: render_diffs(&view["diffs"]),
            status,
        },
        _ => Item::Other {
            id: started.id().unwrap_or_default().to_string(),
            kind: String::new(),
            title: String::new(),
            output: result_text(message),
            status,
        },
    }
}

/// The text the model itself received. It is what a reader wants when no card
/// was produced, and it is the only thing a failed call leaves behind.
fn result_text(message: &Value) -> Option<String> {
    // The blocks are wrapped in one `tool-result` block; the useful text is one
    // level in, which is also where a presenter expects to be handed them.
    let text = message["content"]
        .as_array()?
        .iter()
        .filter_map(|block| block["content"].as_array())
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");

    (!text.trim().is_empty()).then_some(text)
}

fn diff_paths(diffs: &Value) -> String {
    diffs
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["path"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// A unified-diff body for the reviewable pane. The card carries whole before
/// and after texts rather than hunks, so the body is assembled here.
fn render_diffs(diffs: &Value) -> Option<String> {
    let entries = diffs.as_array()?;
    let mut body = String::new();

    for entry in entries {
        let path = entry["path"].as_str().unwrap_or("(unknown)");
        // A create has no prior content, which the card states as null rather
        // than as an empty string.
        let old = entry["oldText"].as_str().unwrap_or_default();
        let new = entry["newText"].as_str().unwrap_or_default();

        body.push_str(&format!("--- {path}\n+++ {path}\n"));
        for line in old.lines() {
            body.push_str(&format!("-{line}\n"));
        }
        for line in new.lines() {
            body.push_str(&format!("+{line}\n"));
        }
    }

    (!body.is_empty()).then_some(body)
}

fn map_tool_call(data: &Value, view: &Value, tools: &mut ToolTracker) -> Vec<Event> {
    let Some(call_id) = data["callId"].as_str() else {
        return Vec::new();
    };

    let item = started_tool_item(data, view);
    tools.started.insert(call_id.to_string(), item.clone());

    vec![Event::ItemStarted(item)]
}

fn map_tool_result(data: &Value, view: &Value, tools: &mut ToolTracker) -> Vec<Event> {
    let message = &data["message"];
    let call_id = message["source"]["callId"]
        .as_str()
        .or_else(|| message["content"][0]["toolCallId"].as_str());
    let Some(call_id) = call_id else {
        return Vec::new();
    };

    // A result with no call is one whose start this session never saw, which
    // happens when a tab attaches to a session mid-turn.
    let Some(started) = tools.started.remove(call_id) else {
        return Vec::new();
    };

    let failed = message["content"][0]["isError"] == Value::Bool(true);

    vec![Event::ItemCompleted(completed_tool_item(
        &started, view, message, failed,
    ))]
}

pub(crate) fn map_session_event(
    event: &Value,
    view: &Value,
    tools: &mut ToolTracker,
) -> Vec<Event> {
    let data = &event["data"];

    match event["type"].as_str() {
        Some("tool/call") => map_tool_call(data, &view["view"], tools),
        Some("tool/result") => map_tool_result(data, &view["view"], tools),
        Some("turn/start") => vec![Event::TurnStarted],
        Some("turn/end") => vec![Event::TurnCompleted {
            error: turn_failure(&data["reason"]),
        }],
        Some("assistant/chunk") => map_chunk(data),
        Some("assistant/message") => map_completed_message(data),
        Some("user/message") => map_user_message(data),
        Some("compaction/start") => vec![Event::CompactionStarted],
        Some("compaction/summary") => map_compaction_summary(data),
        Some("compaction/end") => vec![Event::CompactionFinished {
            error: data["error"].as_str().map(str::to_string),
        }],
        Some("todo/write") => map_todo_write(event, data),
        Some("llm/retry") => map_retry(data),
        // The wait is over and the next attempt is starting, which looks like
        // ordinary work again.
        Some("llm/retry-started") => vec![Event::StatusDetail(None)],
        _ => Vec::new(),
    }
}

/// A provider request that failed and will be tried again.
///
/// This is the one thing the working row cannot show on its own: the turn is
/// waiting rather than thinking, and the elapsed time climbs identically either
/// way while the token count sits still. The delay is left out because it is a
/// countdown, and a figure that stops being true a second after it is drawn
/// reads as worse information than none.
fn map_retry(data: &Value) -> Vec<Event> {
    let attempt = data["retry"].as_u64().unwrap_or(1);
    let total = data["maxRetries"].as_u64().unwrap_or(attempt);
    // The provider's own sentence says the most; its neutral code is the
    // fallback because it at least separates a rate limit from an outage.
    let reason = data["failure"]["message"]
        .as_str()
        .or_else(|| data["failure"]["code"].as_str())
        .unwrap_or("a provider failure");

    vec![Event::StatusDetail(Some(TurnActivity::Retrying {
        attempt,
        total,
        reason: reason.to_string(),
    }))]
}

/// The finished compaction boundary.
///
/// This is the record, not `compaction/end`: the summary event is written only
/// once a compaction produced one, so a failed attempt closes its transaction
/// without leaving a row claiming the conversation was rewritten.
///
/// The replacement text the conversation continues from arrives separately as a
/// `user/message` carrying the checkpoint's plugin source, which the user-message
/// mapping already declines to render as something the user wrote.
fn map_compaction_summary(data: &Value) -> Vec<Event> {
    let Some(id) = data["compactionId"].as_str() else {
        return Vec::new();
    };

    let summary = data["summary"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .filter(|text| !text.is_empty());

    vec![Event::ItemCompleted(Item::Compaction {
        id: id.to_string(),
        detail: Compaction {
            // A manual compaction records the command that asked for it; an
            // automatic one reaches its own threshold and names nothing.
            trigger: Some(if data["sourceCommandId"].is_string() {
                CompactionTrigger::Manual
            } else {
                CompactionTrigger::Automatic
            }),
            // The harness prices the range it replaced rather than the context
            // before and after, so only the replaced side is knowable here.
            pre_tokens: data["shadowedTokenCount"].as_u64(),
            post_tokens: None,
            messages_summarized: data["shadowedSeqs"]
                .as_array()
                .map(|seqs| seqs.len() as u64),
            user_context: None,
            summary,
        },
    })]
}

/// The agent's task list, restated in full on every write.
///
/// It is rendered as the same checklist shape the other harnesses' task tool
/// produces, so the transcript's progress tally reads it without a second
/// vocabulary. Each write is its own row because the list is a snapshot of a
/// moment, and an earlier one stays true about the moment it described.
fn map_todo_write(event: &Value, data: &Value) -> Vec<Event> {
    let checklist: String = data["todos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|todo| {
            let content = todo["content"].as_str()?;
            let mark = if todo["status"] == "completed" {
                "x"
            } else {
                " "
            };
            Some(format!("- [{mark}] {content}\n"))
        })
        .collect();

    if checklist.is_empty() {
        return Vec::new();
    }

    vec![Event::ItemCompleted(Item::Other {
        id: format!("todo:{}", event["seq"].as_u64().unwrap_or_default()),
        kind: "TodoWrite".to_string(),
        title: "Update todo list".to_string(),
        output: Some(checklist),
        status: Some(COMPLETED.to_string()),
    })]
}

/// A turn ends completed, aborted by someone, or failed. Only a failure carries
/// text into the transcript; a user abort is a normal outcome that the tab
/// presents as an interruption rather than an error.
fn turn_failure(reason: &Value) -> Option<String> {
    match reason["kind"].as_str()? {
        "completed" | "aborted" => None,
        other => Some(reason["message"].as_str().unwrap_or(other).to_string()),
    }
}

/// Streaming deltas carry no message id, only their position within the turn.
/// The completed message that follows carries its blocks in the same order, so
/// the position is what lets a streamed row and its completion meet.
fn block_id(data: &Value, index: u64) -> String {
    let turn = data["turn"].as_u64().unwrap_or_default();
    let step = data["step"].as_u64().unwrap_or_default();

    format!("{turn}:{step}:{index}")
}

fn map_chunk(data: &Value) -> Vec<Event> {
    let chunk = &data["chunk"];
    let Some(index) = chunk["index"].as_u64() else {
        return Vec::new();
    };
    let item_id = block_id(data, index);

    match chunk["type"].as_str() {
        // A block announces itself before its first delta, which is what lets
        // an empty row appear immediately rather than at the first token.
        Some("block-start") => match chunk["blockType"].as_str() {
            Some("reasoning") => vec![Event::ItemStarted(Item::Reasoning {
                id: item_id,
                summary: None,
            })],
            Some("text") => vec![Event::ItemStarted(Item::AgentMessage {
                id: item_id,
                text: None,
            })],
            _ => Vec::new(),
        },
        Some("reasoning-delta") => match chunk["text"].as_str() {
            Some(delta) => vec![Event::ReasoningSummaryDelta {
                item_id,
                delta: delta.to_string(),
            }],
            None => Vec::new(),
        },
        Some("text-delta") => match chunk["text"].as_str() {
            Some(delta) => vec![Event::AgentMessageDelta {
                item_id,
                delta: delta.to_string(),
            }],
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The authoritative form of everything the step streamed. Completing each
/// block by its position lets the transcript reconcile with what it already
/// showed instead of appending a duplicate.
///
/// A turn the user stopped never produces one of these, so the streamed rows
/// have to stand on their own.
fn map_completed_message(data: &Value) -> Vec<Event> {
    let Some(blocks) = data["message"]["content"].as_array() else {
        return Vec::new();
    };

    blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            let id = block_id(data, index as u64);
            match block["type"].as_str()? {
                "reasoning" => Some(Event::ItemCompleted(Item::Reasoning {
                    id,
                    summary: block["text"].as_str().map(str::to_string),
                })),
                "text" => Some(Event::ItemCompleted(Item::AgentMessage {
                    id,
                    text: block["text"].as_str().map(str::to_string),
                })),
                // Tool calls are part of the same message. They are not
                // transcript rows in this integration yet, and rendering them
                // as assistant text would be worse than omitting them.
                _ => None,
            }
        })
        .collect()
}

/// One prompt produces several `user/message` events: the user's own, plus the
/// instructions, plugin context, and skill catalog the harness injects around
/// it. Only the first is something the user wrote, and showing the rest would
/// put three messages the user never sent into every turn.
fn map_user_message(data: &Value) -> Vec<Event> {
    if data["source"]["kind"].as_str() != Some("user") {
        return Vec::new();
    }

    let text = data["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .filter(|text| !text.is_empty());

    match text {
        Some(text) => vec![Event::ItemStarted(Item::UserMessage { text: Some(text) })],
        None => Vec::new(),
    }
}
