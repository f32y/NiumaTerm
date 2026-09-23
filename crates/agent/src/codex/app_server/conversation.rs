#[cfg(test)]
#[path = "generation_tests.rs"]
mod generation_tests;

use std::collections::VecDeque;
use std::time::Instant;

use serde_json::Value;

use crate::chat::{Event, GenerationSample};
use crate::codex::app_server::compaction::{
    CompactionState, compaction_completed, compaction_started,
};
use crate::codex::app_server::progress::{PLAN_RESTORED, goal_status, task_list};
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
pub(super) struct ThreadState {
    pub(super) thread_id: Option<String>,
    pub(super) current_turn: Option<String>,

    /// Approval requests not answered yet, in arrival order, with the text
    /// that describes each. The parent and its child agents can each be
    /// waiting on one at the same time.
    approvals: VecDeque<(u64, String)>,

    /// The request the user is looking at. The approval surface shows one at
    /// a time; the next one appears once this one is answered or cleared.
    shown_approval: Option<u64>,

    pub(super) questions: QuestionState,
    pub(super) compaction: CompactionState,
    turn_output_usage: TurnOutputUsage,
    generation_span: Option<(Instant, Instant)>,
    pub(super) goal_revision: u64,
    pub(super) plan_revision: u64,
}

impl ThreadState {
    /// Drop what belonged to the thread being left: its running turn, the
    /// approval and questions it asked, and its compaction in progress. The
    /// next thread, or none after the host exits, owes answers to none of it.
    pub(super) fn end_thread(&mut self) {
        self.current_turn = None;

        self.approvals.clear();

        self.shown_approval = None;
        self.questions = QuestionState::default();
        self.generation_span = None;

        self.compaction.reset_thread();
    }

    pub(super) fn has_pending_approval(&self) -> bool {
        !self.approvals.is_empty()
    }

    /// Queue approval request `rpc_id`, returning the event that shows it
    /// when no other approval is on screen.
    pub(super) fn request_approval(&mut self, rpc_id: u64, description: String) -> Option<Event> {
        self.approvals.push_back((rpc_id, description));

        self.next_approval()
    }

    /// The request on screen, for the user's answer.
    pub(super) fn shown_approval(&self) -> Option<u64> {
        self.shown_approval
    }

    /// Forget request `rpc_id` after its answer went out.
    pub(super) fn answered_approval(&mut self, rpc_id: u64) {
        self.approvals.retain(|(id, _)| *id != rpc_id);

        if self.shown_approval == Some(rpc_id) {
            self.shown_approval = None;
        }
    }

    /// Show the oldest waiting approval once nothing else is on screen.
    pub(super) fn next_approval(&mut self) -> Option<Event> {
        if self.shown_approval.is_some() {
            return None;
        }

        let (id, description) = self.approvals.front()?;

        self.shown_approval = Some(*id);

        Some(Event::ApprovalRequested {
            description: description.clone(),
        })
    }

    pub(super) fn on_notification(&mut self, method: &str, params: &Value) -> Vec<Event> {
        if self.current_turn.is_some()
            && matches!(
                method,
                "item/agentMessage/delta"
                    | "item/reasoning/summaryTextDelta"
                    | "item/reasoning/textDelta"
            )
            && params["delta"]
                .as_str()
                .is_some_and(|delta| !delta.is_empty())
        {
            let now = Instant::now();

            self.generation_span.get_or_insert((now, now)).1 = now;
        }

        match method {
            "turn/plan/updated" => {
                self.plan_revision += 1;

                vec![Event::TaskListUpdated(task_list(params))]
            }
            PLAN_RESTORED => {
                if params["threadId"].as_str() != self.thread_id.as_deref()
                    || params["revision"].as_u64() != Some(self.plan_revision)
                {
                    return Vec::new();
                }

                vec![Event::TaskListUpdated(task_list(&params["value"]))]
            }
            "thread/goal/updated" => {
                self.goal_revision += 1;

                vec![Event::GoalUpdated(goal_status(&params["goal"]))]
            }
            "thread/goal/cleared" => {
                self.goal_revision += 1;

                vec![Event::GoalUpdated(None)]
            }
            "turn/started" => {
                self.generation_span = None;
                self.current_turn = params["turn"]["id"].as_str().map(str::to_owned);

                self.turn_output_usage.begin_turn();

                let mut events = vec![Event::TurnStarted];

                if let Some(id) = &self.current_turn {
                    events.push(Event::ProviderTurnAccepted { id: id.clone() });
                }

                events
            }
            "turn/completed" => {
                self.generation_span = None;

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

                let previous_total = self.turn_output_usage.latest_total;

                let turn_output_tokens = usage
                    .cumulative
                    .and_then(|usage| usage.breakdown.output_tokens)
                    .zip(usage.current.output_tokens)
                    .and_then(|(total, last)| self.turn_output_usage.observe(total, last, active));

                let mut events = vec![Event::ContextWindowUpdated(usage)];

                if let Some(output_tokens) = turn_output_tokens {
                    events.push(Event::TurnOutputTokensUpdated(output_tokens));
                }

                // Usage can be repeated by quota updates while the next
                // response is streaming. Only a new total closes a sample.
                if active
                    && let Some(total) = self.turn_output_usage.latest_total
                    && previous_total != Some(total)
                    && let Some((started, ended)) = self.generation_span.take()
                    && let Some(output_tokens) = usage.current.output_tokens
                    && previous_total.is_none_or(|previous| total > previous)
                {
                    events.push(Event::GenerationCompleted(GenerationSample {
                        response_id: format!(
                            "{}:{total}",
                            self.current_turn.as_deref().unwrap_or_default()
                        ),
                        output_tokens,
                        // Tool execution can overlap a response and delay
                        // usage delivery. End at the last model delta so
                        // waiting for tools cannot lower the reading.
                        elapsed: ended.saturating_duration_since(started),
                        estimated: true,
                    }));
                }

                events
            }
            "item/started" => {
                let item = &params["item"];

                if item["type"].as_str() == Some("contextCompaction") {
                    self.generation_span = None;

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
                let request = params["requestId"].as_u64();

                // Fires when an approval is answered or cleared by turn
                // lifecycle. A child agent's approval names the child's
                // thread, so approvals match by request id alone.
                if let Some(id) = request
                    && self.approvals.iter().any(|(pending, _)| *pending == id)
                {
                    let shown = self.shown_approval == Some(id);

                    self.answered_approval(id);

                    return if shown {
                        vec![Event::ApprovalResolved]
                    } else {
                        Vec::new()
                    };
                }

                if params["threadId"].as_str() != self.thread_id.as_deref() {
                    return Vec::new();
                }

                request
                    .and_then(|id| self.questions.resolve_request(id))
                    .into_iter()
                    .collect()
            }
            "error" => {
                self.generation_span = None;

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
