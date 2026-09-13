use serde_json::Value;

use crate::chat::Event;
use crate::codex::app_server::compaction::{
    CompactionState, compaction_completed, compaction_started,
};
use crate::codex::app_server::protocol::{delta_event, parse_context_window_usage, parse_item};
use crate::codex::app_server::questions::QuestionState;

#[derive(Default)]
pub(super) struct TurnOutputUsage {
    latest_total: Option<u64>,
    baseline: Option<u64>,
}

impl TurnOutputUsage {
    pub(super) fn begin_turn(&mut self) {
        self.baseline = self.latest_total;
    }

    pub(super) fn finish_turn(&mut self) {
        self.baseline = None;
    }

    pub(super) fn observe(&mut self, total: u64, last: u64, active: bool) -> Option<u64> {
        self.latest_total = Some(total);

        if !active {
            return None;
        }

        let inferred_baseline = total.saturating_sub(last);
        let baseline = self.baseline.get_or_insert(inferred_baseline);

        if total < *baseline {
            *baseline = inferred_baseline;
        }

        Some(total.saturating_sub(*baseline))
    }
}

/// State belonging to the parent conversation, independent of host routing.
#[derive(Default)]
pub(super) struct ConversationState {
    pub(super) thread_id: Option<String>,
    pub(super) current_turn: Option<String>,
    pub(super) pending_approval: Option<u64>,
    pub(super) questions: QuestionState,
    pub(super) compaction: CompactionState,
    turn_output_usage: TurnOutputUsage,
}

impl ConversationState {
    pub(super) fn on_notification(&mut self, method: &str, params: &Value) -> Vec<Event> {
        match method {
            "turn/started" => {
                self.current_turn = params["turn"]["id"].as_str().map(str::to_owned);
                self.turn_output_usage.begin_turn();

                let mut events = vec![Event::TurnStarted];

                if let Some(id) = &self.current_turn {
                    events.push(Event::ProviderTurnAccepted { id: id.clone() });
                }

                events
            }

            "turn/completed" => {
                let mut events = self
                    .questions
                    .end_turn(params["turn"]["id"].as_str().unwrap_or_default());

                self.current_turn = None;
                self.turn_output_usage.finish_turn();

                let error = (params["turn"]["status"].as_str() == Some("failed"))
                    .then(|| params["turn"]["error"]["message"].as_str())
                    .flatten()
                    .map(str::to_owned);

                self.compaction.clear_incomplete();

                events.push(Event::TurnCompleted {
                    error: error.clone(),
                });

                if let Some(id) = params["turn"]["id"].as_str() {
                    events.push(Event::ProviderTurnFinished {
                        id: id.to_owned(),
                        error: error.or_else(|| {
                            (params["turn"]["status"].as_str() == Some("interrupted"))
                                .then(|| "The provider turn was interrupted.".into())
                        }),
                    });
                }

                events
            }

            "thread/tokenUsage/updated" => {
                let Some(usage) = parse_context_window_usage(&params["tokenUsage"]) else {
                    return Vec::new();
                };

                self.compaction.update_usage(usage);

                let active = params["turnId"]
                    .as_str()
                    .is_some_and(|turn_id| self.current_turn.as_deref() == Some(turn_id));

                let turn_output_tokens = usage
                    .cumulative
                    .and_then(|usage| usage.breakdown.output_tokens)
                    .zip(usage.current.output_tokens)
                    .and_then(|(total, last)| self.turn_output_usage.observe(total, last, active));

                let mut events = vec![Event::ContextWindowUpdated(usage)];

                if let Some(output_tokens) = turn_output_tokens {
                    events.push(Event::TurnOutputTokensUpdated(output_tokens));
                }

                events
            }

            "item/started" => {
                let item = &params["item"];

                if item["type"].as_str() == Some("contextCompaction") {
                    return compaction_started(&mut self.compaction, item);
                }

                parse_item(item)
                    .map(Event::ItemStarted)
                    .into_iter()
                    .collect()
            }

            "item/completed" => {
                let item = &params["item"];

                if item["type"].as_str() == Some("contextCompaction") {
                    return compaction_completed(&mut self.compaction, item);
                }

                let mut events: Vec<Event> = parse_item(item)
                    .map(Event::ItemCompleted)
                    .into_iter()
                    .collect();

                events.extend(self.questions.observe_message(item));

                events
            }

            "item/agentMessage/delta" => delta_event(params, |item_id, delta| {
                Event::AgentMessageDelta { item_id, delta }
            }),

            "item/reasoning/summaryTextDelta" => delta_event(params, |item_id, delta| {
                Event::ReasoningSummaryDelta { item_id, delta }
            }),

            // Raw reasoning tokens stream under their own method and append to
            // the same text as the summary deltas, because a model that emits
            // raw tokens is the one that emits no summary. A model that sent
            // both would interleave them for the rest of the item, since a
            // stream cannot retract text it already appended and a completed
            // item only fills reasoning text that streamed empty.
            "item/reasoning/textDelta" => delta_event(params, |item_id, delta| {
                Event::ReasoningSummaryDelta { item_id, delta }
            }),

            "item/commandExecution/outputDelta" => delta_event(params, |item_id, delta| {
                Event::CommandOutputDelta { item_id, delta }
            }),

            "serverRequest/resolved" => {
                if params["threadId"].as_str() != self.thread_id.as_deref() {
                    return Vec::new();
                }

                if let Some(event) = params["requestId"]
                    .as_u64()
                    .and_then(|id| self.questions.resolve_request(id))
                {
                    return vec![event];
                }

                // Fires when a pending approval is answered or cleared by
                // turn lifecycle — tear down the approval UI either way.
                if self.pending_approval.is_some()
                    && self.pending_approval == params["requestId"].as_u64()
                {
                    self.pending_approval = None;

                    return vec![Event::ApprovalResolved];
                }

                Vec::new()
            }

            "error" => {
                let message = params["error"]["message"]
                    .as_str()
                    .or_else(|| params["message"].as_str())
                    .unwrap_or("unknown Codex error")
                    .to_string();

                vec![Event::Error {
                    message,
                    fatal: false,
                }]
            }

            // The status carries a protocol token rather than a sentence, and
            // the working row it would reach shows text to the user. Child-agent
            // rows read the same notification through their own reducer, which
            // maps it to a lifecycle state instead of showing the word.
            "thread/status/changed" => Vec::new(),
            _ => Vec::new(),
        }
    }
}
