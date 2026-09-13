use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use crate::chat::{ContextComposition, ContextWindowUsage, Event, Item, TokenUsageBreakdown};
use crate::claude_code::compaction::{compaction_metadata, parse_compaction};
use crate::claude_code::stream_json::parse::{
    claude_context_window, context_window_usage, parse_claude_usage, update_claude_output,
};
use crate::claude_code::tool_items::{complete_tool_item, tool_item};

#[derive(Default)]
pub(super) struct TurnOutputUsage {
    completed_responses: u64,
    current_response: Option<u64>,
}

impl TurnOutputUsage {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn start_response(&mut self, output_tokens: u64) -> u64 {
        self.completed_responses = self
            .completed_responses
            .saturating_add(self.current_response.take().unwrap_or(0));

        self.current_response = Some(output_tokens);

        self.total()
    }

    pub(super) fn update_response(&mut self, output_tokens: u64) -> u64 {
        self.current_response = Some(output_tokens);

        self.total()
    }

    pub(super) fn total(&self) -> u64 {
        self.completed_responses
            .saturating_add(self.current_response.unwrap_or(0))
    }
}

#[derive(Default)]
pub(super) struct TranscriptState {
    /// Streamed content blocks of the in-flight assistant message, keyed by
    /// their stream index, so text/thinking deltas route to transcript items.
    open_blocks: HashMap<u64, String>,

    /// Streamed text/thinking items not yet finalized by an `assistant`
    /// snapshot. Snapshots arrive per completed block in stream order, so
    /// FIFO matching by kind pairs each snapshot with its streamed item.
    open_texts: VecDeque<String>,

    open_thinkings: VecDeque<String>,

    /// Started tool items by `tool_use_id`; the matching `tool_result` block
    /// completes them with output and status.
    pending_tools: HashMap<String, Item>,

    item_seq: u64,

    /// The most recent assistant message's input/output accounting represents
    /// the live context, unlike result-level totals which may sum retries and
    /// tool-loop iterations.
    context_usage: Option<TokenUsageBreakdown>,

    last_turn_usage: Option<TokenUsageBreakdown>,
    context_window: Option<u64>,
    turn_output_usage: TurnOutputUsage,
}

impl TranscriptState {
    pub(super) fn begin_turn(&mut self) {
        self.turn_output_usage.reset();
    }

    pub(super) fn finish_turn(&mut self, message: &Value) -> Vec<Event> {
        let mut events = Vec::new();

        if let Some(max_tokens) = claude_context_window(&message["modelUsage"]) {
            self.context_window = Some(max_tokens);
        }

        self.last_turn_usage = parse_claude_usage(&message["usage"]);

        if let Some(usage) = self.context_window_usage() {
            events.push(Event::ContextWindowUpdated(usage));
        }

        if let Some(output_tokens) = self.last_turn_usage.and_then(|usage| usage.output_tokens) {
            events.push(Event::TurnOutputTokensUpdated(output_tokens));
        }

        events
    }

    pub(super) fn apply_composition(
        &mut self,
        composition: &ContextComposition,
    ) -> Option<ContextWindowUsage> {
        let filled = window_from_composition(self.context_usage, composition)?;

        self.context_usage = Some(filled);
        self.context_window = self.context_window.or(composition.max_tokens);

        self.context_window_usage()
    }

    /// The post-compaction boundary. Live it carries only the token accounting:
    /// the replacement summary is written to the transcript file and marked
    /// visible there only, so a resumed thread shows it and this one does not.
    pub(super) fn on_compact_boundary(&mut self, message: &Value) -> Vec<Event> {
        let detail = parse_compaction(compaction_metadata(message));

        let id = match message["uuid"].as_str() {
            Some(uuid) => format!("compaction-{uuid}"),
            None => self.alloc_item_id("compaction"),
        };

        let post_tokens = detail.post_tokens;

        let mut events = vec![
            Event::CompactionFinished { error: None },
            Event::ItemCompleted(Item::Compaction { id, detail }),
        ];

        // Compaction replaces the prompt, so the live context is this size from
        // here on. Without the correction the gauge keeps showing the
        // pre-compaction total until the next assistant message reports usage,
        // which is exactly when the boundary row claims space was reclaimed.
        if let Some(post_tokens) = post_tokens {
            self.context_usage = Some(TokenUsageBreakdown::total_only(post_tokens));

            if let Some(usage) = self.context_window_usage() {
                events.push(Event::ContextWindowUpdated(usage));
            }
        }

        events
    }

    fn alloc_item_id(&mut self, prefix: &str) -> String {
        self.item_seq += 1;

        format!("{prefix}-{}", self.item_seq)
    }

    pub(super) fn on_stream_event(&mut self, message: &Value) -> Vec<Event> {
        // Subagent (Task tool) internals stream with a parent id; the parent
        // tool row already represents them in the transcript.
        if !message["parent_tool_use_id"].is_null() {
            return Vec::new();
        }

        let event = &message["event"];
        let index = event["index"].as_u64();

        match event["type"].as_str() {
            Some("message_start") => {
                self.open_blocks.clear();
                self.open_texts.clear();
                self.open_thinkings.clear();

                self.context_usage = parse_claude_usage(&event["message"]["usage"]);

                let turn_output_tokens = self.turn_output_usage.start_response(
                    self.context_usage
                        .and_then(|usage| usage.output_tokens)
                        .unwrap_or(0),
                );

                let mut events = self
                    .context_window_usage()
                    .map(Event::ContextWindowUpdated)
                    .into_iter()
                    .collect::<Vec<_>>();

                events.push(Event::TurnOutputTokensUpdated(turn_output_tokens));

                events
            }

            Some("message_delta") => {
                let turn_output_tokens =
                    event["usage"]["output_tokens"]
                        .as_u64()
                        .map(|output_tokens| {
                            update_claude_output(&mut self.context_usage, output_tokens);

                            self.turn_output_usage.update_response(output_tokens)
                        });

                let mut events = self
                    .context_window_usage()
                    .map(Event::ContextWindowUpdated)
                    .into_iter()
                    .collect::<Vec<_>>();

                if let Some(output_tokens) = turn_output_tokens {
                    events.push(Event::TurnOutputTokensUpdated(output_tokens));
                }

                events
            }

            Some("content_block_start") => {
                let Some(index) = index else {
                    return Vec::new();
                };

                match event["content_block"]["type"].as_str() {
                    Some("text") => {
                        let id = self.alloc_item_id("text");

                        self.open_blocks.insert(index, id.clone());
                        self.open_texts.push_back(id.clone());

                        vec![Event::ItemStarted(Item::AgentMessage {
                            id,
                            text: None,
                            questions: None,
                        })]
                    }

                    Some("thinking") => {
                        let id = self.alloc_item_id("thinking");

                        self.open_blocks.insert(index, id.clone());
                        self.open_thinkings.push_back(id.clone());

                        vec![Event::ItemStarted(Item::Reasoning { id, summary: None })]
                    }

                    // Tool-use blocks stream their input as JSON fragments;
                    // the item is emitted from the `assistant` snapshot where
                    // the input is complete.
                    _ => Vec::new(),
                }
            }

            Some("content_block_delta") => {
                let Some(item_id) = index.and_then(|i| self.open_blocks.get(&i)).cloned() else {
                    return Vec::new();
                };

                let delta = &event["delta"];

                match delta["type"].as_str() {
                    Some("text_delta") => delta["text"]
                        .as_str()
                        .map(|text| Event::AgentMessageDelta {
                            item_id,
                            delta: text.to_string(),
                        })
                        .into_iter()
                        .collect(),

                    Some("thinking_delta") => delta["thinking"]
                        .as_str()
                        .map(|text| Event::ReasoningSummaryDelta {
                            item_id,
                            delta: text.to_string(),
                        })
                        .into_iter()
                        .collect(),

                    _ => Vec::new(),
                }
            }

            _ => Vec::new(),
        }
    }

    /// An `assistant` snapshot finalizes each content block it carries: text
    /// and thinking blocks overwrite their streamed item with the
    /// authoritative full text (or create it when partial messages were
    /// missed), tool-use blocks become started tool items.
    pub(super) fn on_assistant(&mut self, message: &Value) -> Vec<Event> {
        if !message["parent_tool_use_id"].is_null() {
            return Vec::new();
        }

        let Some(blocks) = message["message"]["content"].as_array() else {
            return Vec::new();
        };

        let mut events = Vec::new();

        if let Some(usage) = parse_claude_usage(&message["message"]["usage"]) {
            self.context_usage = Some(usage);

            if let Some(snapshot) = self.context_window_usage() {
                events.push(Event::ContextWindowUpdated(snapshot));
            }

            if let Some(output_tokens) = usage.output_tokens {
                events.push(Event::TurnOutputTokensUpdated(
                    self.turn_output_usage.update_response(output_tokens),
                ));
            }
        }

        for block in blocks {
            match block["type"].as_str() {
                Some("text") => {
                    let id = self
                        .open_texts
                        .pop_front()
                        .unwrap_or_else(|| self.alloc_item_id("text"));

                    events.push(Event::ItemCompleted(Item::AgentMessage {
                        id,
                        text: block["text"].as_str().map(str::to_owned),
                        questions: None,
                    }));
                }

                Some("thinking") => {
                    let id = self
                        .open_thinkings
                        .pop_front()
                        .unwrap_or_else(|| self.alloc_item_id("thinking"));

                    events.push(Event::ItemCompleted(Item::Reasoning {
                        id,
                        summary: block["thinking"].as_str().map(str::to_owned),
                    }));
                }

                Some("tool_use") | Some("server_tool_use") | Some("mcp_tool_use") => {
                    let Some(id) = block["id"].as_str() else {
                        continue;
                    };

                    let item = tool_item(
                        id,
                        block["name"].as_str().unwrap_or("tool"),
                        &block["input"],
                    );

                    self.pending_tools.insert(id.to_string(), item.clone());
                    events.push(Event::ItemStarted(item));
                }

                _ => {}
            }
        }

        events
    }

    /// `user` messages in the stream carry tool results; each one completes
    /// its started tool item with output and success/failure status.
    pub(super) fn on_tool_results(&mut self, message: &Value) -> Vec<Event> {
        let Some(blocks) = message["message"]["content"].as_array() else {
            return Vec::new();
        };

        let mut events = Vec::new();

        for block in blocks {
            if block["type"].as_str() != Some("tool_result") {
                continue;
            }

            let Some(id) = block["tool_use_id"].as_str() else {
                continue;
            };

            let Some(started) = self.pending_tools.remove(id) else {
                continue;
            };

            events.push(Event::ItemCompleted(complete_tool_item(started, block)));
        }

        events
    }

    fn context_window_usage(&self) -> Option<ContextWindowUsage> {
        context_window_usage(
            self.context_usage,
            self.last_turn_usage,
            self.context_window,
        )
    }
}

/// Whether a context breakdown should stand in for the window's own
/// accounting. Live accounting names each category, so it is always the better
/// answer; the breakdown only fills the gap before any has arrived.
pub(super) fn window_from_composition(
    live: Option<TokenUsageBreakdown>,
    composition: &ContextComposition,
) -> Option<TokenUsageBreakdown> {
    (live.is_none() && composition.used_tokens > 0)
        .then(|| TokenUsageBreakdown::total_only(composition.used_tokens))
}
