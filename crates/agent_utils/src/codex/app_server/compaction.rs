use std::mem::take;

use serde_json::Value;

use crate::chat::{
    Compaction, CompactionTrigger, ContextWindowUsage, Event, Item, SlashCommandOutcome,
};

#[derive(Debug)]
pub(super) struct ActiveCompaction {
    id: String,
    trigger: CompactionTrigger,
    pre_tokens: Option<u64>,
}

/// Correlates the separate item and token-usage streams that describe one
/// Codex context rewrite. The app-server item itself contains only an id, so
/// live trigger and accounting details come from client intent and adjacent
/// usage snapshots.
#[derive(Default)]
pub(super) struct CompactionState {
    latest_usage: Option<ContextWindowUsage>,
    manual_pending: bool,
    pub(super) active: Option<ActiveCompaction>,
}

impl CompactionState {
    pub(super) fn update_usage(&mut self, usage: ContextWindowUsage) {
        self.latest_usage = Some(usage);
    }

    pub(super) fn request_manual(&mut self) {
        self.manual_pending = true;
    }

    pub(super) fn reject_manual_request(&mut self) {
        self.manual_pending = false;
    }

    fn start(&mut self, id: &str) -> bool {
        if self.active.as_ref().is_some_and(|active| active.id == id) {
            return false;
        }

        let trigger = if take(&mut self.manual_pending) {
            CompactionTrigger::Manual
        } else {
            CompactionTrigger::Automatic
        };

        self.active = Some(ActiveCompaction {
            id: id.to_string(),
            trigger,
            pre_tokens: self.latest_usage.map(ContextWindowUsage::used_tokens),
        });

        true
    }

    fn finish(&mut self, id: &str) -> Compaction {
        let active = self.active.take().filter(|active| active.id == id);
        let (trigger, pre_tokens) = match active {
            Some(active) => (active.trigger, active.pre_tokens),
            None => {
                let trigger = if take(&mut self.manual_pending) {
                    CompactionTrigger::Manual
                } else {
                    CompactionTrigger::Automatic
                };
                (trigger, None)
            }
        };
        let post_tokens = pre_tokens.and_then(|pre_tokens| {
            self.latest_usage
                .map(ContextWindowUsage::used_tokens)
                .filter(|post_tokens| *post_tokens < pre_tokens)
        });

        Compaction {
            trigger: Some(trigger),
            pre_tokens,
            post_tokens,
            ..Compaction::default()
        }
    }

    /// A failed or interrupted turn cannot leave manual intent attached to a
    /// future provider-initiated compaction. The latest usage remains useful
    /// as the baseline for the next turn.
    pub(super) fn clear_incomplete(&mut self) {
        self.manual_pending = false;
        self.active = None;
    }

    pub(super) fn reset_thread(&mut self) {
        *self = Self::default();
    }
}

pub(super) fn is_legacy_compaction_notification(method: &str) -> bool {
    method == "thread/compacted"
}

pub(super) fn compaction_started(state: &mut CompactionState, item: &Value) -> Vec<Event> {
    let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
        return Vec::new();
    };

    state
        .start(id)
        .then_some(Event::CompactionStarted)
        .into_iter()
        .collect()
}

pub(super) fn compaction_completed(state: &mut CompactionState, item: &Value) -> Vec<Event> {
    let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
        return Vec::new();
    };
    let detail = state.finish(id);
    let manual = detail.trigger == Some(CompactionTrigger::Manual);
    let mut events = vec![
        Event::CompactionFinished { error: None },
        Event::ItemCompleted(Item::Compaction {
            id: id.to_string(),
            detail,
        }),
    ];

    if manual {
        events.push(Event::SlashCommandResult {
            name: "compact".to_string(),
            outcome: SlashCommandOutcome::Completed {
                message: Some("Conversation context compacted.".to_string()),
            },
        });
    }

    events
}
