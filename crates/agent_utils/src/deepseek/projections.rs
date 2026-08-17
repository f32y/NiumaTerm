//! Folding the host's projection units into the snapshots the pane renders.
//!
//! The host publishes each unit as its own frame carrying that unit's whole
//! current value, so a figure the pane shows as one thing arrives here as
//! several independent updates. This holds the latest of each and republishes
//! the combination, which is why it is a tracker rather than a pure mapping.

use serde_json::Value;

use crate::chat::{
    ApprovalPreset, ContextComposition, ContextSegment, ContextUsageScope, ContextWindowUsage,
    Event, ScopedTokenUsage, TokenUsageBreakdown,
};

/// The projection values this session has seen so far.
#[derive(Default)]
pub(crate) struct ProjectionTracker {
    /// Provider-reported totals over the whole log.
    cumulative: Option<TokenUsageBreakdown>,
    /// What the next request's prompt is expected to cost.
    used_tokens: Option<u64>,
    /// Capacity of the route that produced the newest sample.
    context_window: Option<u64>,
}

impl ProjectionTracker {
    /// Fold one frame. Returns `None` for anything that is not a projection
    /// addressed to this session, so a caller can go on to try the frame
    /// against the other mappings.
    pub(crate) fn apply(&mut self, frame: &Value, session_id: &str) -> Option<Vec<Event>> {
        let payload = &frame["payload"];
        if payload["type"] != "session/projection"
            || payload["sessionId"].as_str() != Some(session_id)
        {
            return None;
        }

        Some(self.apply_unit(payload["key"].as_str()?, &payload["value"]))
    }

    /// Fold the whole baseline a history page carries.
    ///
    /// A live push only reports what changed since the session started, so a
    /// tab that read nothing else would show no accounting and no permission
    /// preset until one of them happened to move.
    pub(crate) fn apply_baseline(&mut self, values: &Value) -> Vec<Event> {
        values
            .as_object()
            .into_iter()
            .flatten()
            .flat_map(|(key, value)| self.apply_unit(key, value))
            .collect()
    }

    fn apply_unit(&mut self, key: &str, value: &Value) -> Vec<Event> {
        match key {
            "tokenUsage" => {
                self.cumulative = Some(cumulative_usage(value));
                self.window_event().into_iter().collect()
            }
            "contextPressure" => {
                // `projectedTokens` re-prices what the surface gained since the
                // provider's last sample, which is what makes the figure react
                // to a compaction; the provider-anchored sample is the fallback
                // when the estimate is absent.
                self.used_tokens = value["projectedTokens"]
                    .as_u64()
                    .or_else(|| value["pressureTokens"].as_u64());
                self.context_window = value["contextWindow"].as_u64().filter(|max| *max > 0);
                self.window_event().into_iter().collect()
            }
            "contextBreakdown" => self.composition_event(value).into_iter().collect(),
            "permissions" => permission_presets(value).into_iter().collect(),
            // The host registers whatever projection units the deployment
            // composed; the ones this build does not read are normal traffic.
            _ => Vec::new(),
        }
    }

    /// The window snapshot, once occupancy is known. The cumulative totals are
    /// reported against the same window, so publishing them before the first
    /// provider sample would draw an empty bar beside real token counts.
    fn window_event(&self) -> Option<Event> {
        Some(Event::ContextWindowUpdated(ContextWindowUsage {
            current: TokenUsageBreakdown::total_only(self.used_tokens?),
            cumulative: self.cumulative.map(|breakdown| ScopedTokenUsage {
                scope: ContextUsageScope::Thread,
                breakdown,
            }),
            max_tokens: self.context_window,
        }))
    }

    /// The composition of the next request's prompt.
    ///
    /// The three figures share one heuristic density estimate rather than the
    /// provider's own accounting, so they do not add up to the occupancy figure
    /// beside them. Their sum is still the only honest total for this split,
    /// because it is what the same estimator says the parts come to.
    fn composition_event(&self, value: &Value) -> Option<Event> {
        let segments: Vec<ContextSegment> = [
            ("System prompt", "systemTokens"),
            ("Tools", "toolsTokens"),
            ("Messages", "messageTokens"),
        ]
        .into_iter()
        .filter_map(|(label, field)| {
            Some(ContextSegment {
                label: label.to_string(),
                tokens: value[field].as_u64()?,
                color: None,
                deferred: false,
            })
        })
        .collect();

        if segments.is_empty() {
            return None;
        }

        Some(Event::ContextCompositionUpdated(ContextComposition {
            used_tokens: segments.iter().map(|segment| segment.tokens).sum(),
            max_tokens: self.context_window,
            raw_max_tokens: None,
            auto_compact_threshold: None,
            segments,
        }))
    }
}

/// The presets this session can switch between, and the one it is on.
///
/// The options are the deployment's own preset table rather than a list this
/// build knows, so both travel: a picker built from a hard-coded set would
/// offer values a deployment does not serve and hide the ones it does. The
/// derived `custom` entry appears only while the knobs match no preset, which
/// is why it can be the current value without being switchable to.
fn permission_presets(value: &Value) -> Option<Event> {
    let presets: Vec<ApprovalPreset> = value["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|option| {
            Some(ApprovalPreset {
                value: option["value"].as_str()?.to_string(),
                label: option["name"].as_str().unwrap_or_default().to_string(),
                description: option["description"].as_str().map(str::to_string),
            })
        })
        .collect();

    (!presets.is_empty()).then(|| Event::ApprovalPresets {
        presets,
        current: value["currentValue"].as_str().map(str::to_string),
    })
}

/// The four buckets are disjoint — reasoning tokens are already inside the
/// output count — so the total is their plain sum.
fn cumulative_usage(value: &Value) -> TokenUsageBreakdown {
    let uncached_input = value["uncachedInputTokens"].as_u64().unwrap_or_default();
    let output = value["outputTokens"].as_u64().unwrap_or_default();
    let cache_read = value["cacheReadTokens"].as_u64().unwrap_or_default();
    let cache_write = value["cacheWriteTokens"].as_u64().unwrap_or_default();

    TokenUsageBreakdown {
        total_tokens: uncached_input + output + cache_read + cache_write,
        input_tokens: Some(uncached_input),
        cache_read_input_tokens: Some(cache_read),
        cache_write_input_tokens: Some(cache_write),
        output_tokens: Some(output),
        // The provider reports no separate reasoning figure here, and inventing
        // one from the output total would double-count it.
        reasoning_output_tokens: None,
    }
}
