use std::time::Duration;

use gpui::prelude::*;
use gpui::{App, FontWeight, IntoElement, RenderOnce, Window, div, px};
use gpui_component::hover_card::HoverCard;
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};
use nmt_agent_utils::chat::{
    ContextComposition, ContextSegment, ContextUsageScope, ContextWindowUsage, SessionStats,
    TokenUsageBreakdown,
};
use nmt_i18n::i18n;

use super::transcript::compact_token_count;

#[derive(IntoElement)]
pub(super) struct ContextUsageIndicator {
    usage: ContextWindowUsage,
    /// What fills the window, when the provider measures it. Codex reports
    /// only accounting, so its card shows the accounting alone.
    composition: Option<ContextComposition>,
    /// Whole-log figures for the conversation, when the provider folds them.
    /// They belong beside the accounting because both answer what the
    /// conversation has cost, one in tokens and one in turns and time.
    stats: Option<SessionStats>,
}

impl ContextUsageIndicator {
    pub(super) fn new(
        usage: ContextWindowUsage,
        composition: Option<ContextComposition>,
        stats: Option<SessionStats>,
    ) -> Self {
        Self {
            usage,
            composition,
            stats,
        }
    }
}

/// Wall time as the largest unit that still reads as a duration rather than as
/// a number: a tool that ran for two minutes is more legible as `2m 5s` than as
/// either `125s` or `0.03h`.
fn wall_time_readout(millis: u64) -> String {
    let seconds = millis / 1000;

    match (seconds / 3600, (seconds % 3600) / 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    }
}

/// One row of the composition list: a labelled part of the window with its
/// share of what the provider measured.
#[derive(Clone, Debug, IntoElement, PartialEq)]
struct ContextSegmentRow {
    label: String,
    tokens: u64,
    percent: u64,
    deferred: bool,
}

/// Order segments largest first so the card answers "what is filling this"
/// before it answers "what else is in here", and drop empty ones rather than
/// listing parts that occupy nothing.
fn context_segment_rows(composition: &ContextComposition) -> Vec<ContextSegmentRow> {
    // Percentages are taken against the measured window when the provider
    // reports one, so a segment's share matches the capacity line above it.
    let total = composition
        .max_tokens
        .filter(|max| *max > 0)
        .unwrap_or_else(|| composition.segments.iter().map(|s| s.tokens).sum());

    let mut rows: Vec<ContextSegmentRow> = composition
        .segments
        .iter()
        .filter(|segment| segment.tokens > 0)
        .map(|segment: &ContextSegment| ContextSegmentRow {
            label: segment.label.clone(),
            tokens: segment.tokens,
            percent: if total == 0 {
                0
            } else {
                (segment.tokens as f64 * 100.0 / total as f64).round() as u64
            },
            deferred: segment.deferred,
        })
        .collect();

    rows.sort_by(|left, right| right.tokens.cmp(&left.tokens));
    rows
}

impl RenderOnce for ContextSegmentRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        // A deferred row is dimmed rather than relabelled: its name arrives as
        // the harness renders it, and Claude's already ends in "(deferred)".
        let label_color = if self.deferred {
            muted.opacity(0.72)
        } else {
            foreground.opacity(0.86)
        };

        h_flex()
            .w_full()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(label_color)
                    .child(self.label),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .child(
                        div()
                            .text_color(muted.opacity(0.72))
                            .child(format!("{}%", self.percent)),
                    )
                    .child(
                        div()
                            .text_color(label_color)
                            .child(compact_token_count(self.tokens)),
                    ),
            )
    }
}

#[derive(Clone, Copy, Debug, IntoElement, PartialEq, Eq)]
struct TokenUsageRow {
    label: &'static str,
    tokens: u64,
    nested: bool,
}

fn remaining_context_percent(usage: ContextWindowUsage) -> Option<u64> {
    let max_tokens = usage.max_tokens.filter(|max_tokens| *max_tokens > 0)?;
    let remaining_tokens = max_tokens.saturating_sub(usage.used_tokens());

    Some((remaining_tokens as f64 * 100.0 / max_tokens as f64).round() as u64)
}

/// Share of input the provider served from its cache, measured over the widest
/// scope it reports rather than over the newest request alone.
///
/// A single request is the wrong denominator: every request in a tool loop
/// replays the whole conversation, and only the first one pays to write the new
/// content into the cache. The last request of a turn therefore reads near 100%
/// however much that turn actually cost. Aggregating over the provider's own
/// turn or thread scope keeps those cache writes in the denominator, so the
/// readout moves when the work does.
///
/// Both providers report cached tokens inside `input_tokens` — Claude by
/// folding its read and write counts into the total, Codex natively — so the
/// share cannot exceed 100% and needs no clamp.
pub(super) fn cache_hit_percent(usage: ContextWindowUsage) -> Option<u64> {
    // Older protocol revisions and post-compaction snapshots report a
    // cumulative total without categories; the newest request is then the only
    // breakdown there is.
    usage
        .cumulative
        .and_then(|scoped| cache_hit_of(scoped.breakdown))
        .or_else(|| cache_hit_of(usage.current))
}

fn cache_hit_of(breakdown: TokenUsageBreakdown) -> Option<u64> {
    let input_tokens = breakdown.input_tokens.filter(|tokens| *tokens > 0)?;
    let cache_read = breakdown.cache_read_input_tokens?;

    Some((cache_read as f64 * 100.0 / input_tokens as f64).round() as u64)
}

fn context_indicator_label(usage: ContextWindowUsage) -> String {
    match remaining_context_percent(usage) {
        Some(remaining_percent) => i18n("agent-context-used-left")
            .replace("{tokens}", &compact_token_count(usage.used_tokens()))
            .replace("{percent}", &remaining_percent.to_string()),
        None => i18n("agent-context-used")
            .replace("{tokens}", &compact_token_count(usage.used_tokens())),
    }
}

fn context_capacity_labels(usage: ContextWindowUsage) -> (String, Option<String>) {
    match usage.max_tokens.filter(|max_tokens| *max_tokens > 0) {
        Some(max_tokens) => (
            format!(
                "{} / {}",
                compact_token_count(usage.used_tokens()),
                compact_token_count(max_tokens)
            ),
            remaining_context_percent(usage).map(|percent| {
                i18n("agent-context-percent-left").replace("{percent}", &percent.to_string())
            }),
        ),
        None => (
            i18n("agent-context-used")
                .replace("{tokens}", &compact_token_count(usage.used_tokens())),
            None,
        ),
    }
}

fn token_usage_rows(usage: TokenUsageBreakdown, include_total: bool) -> Vec<TokenUsageRow> {
    let mut rows = Vec::new();

    if include_total {
        rows.push(TokenUsageRow {
            label: i18n("agent-context-total"),
            tokens: usage.total_tokens,
            nested: false,
        });
    }
    if let Some(tokens) = usage.input_tokens {
        rows.push(TokenUsageRow {
            label: i18n("agent-context-input"),
            tokens,
            nested: false,
        });
    }
    if let Some(tokens) = usage.cache_read_input_tokens {
        rows.push(TokenUsageRow {
            label: i18n("agent-context-cache-read"),
            tokens,
            nested: true,
        });
    }
    if let Some(tokens) = usage.cache_write_input_tokens {
        rows.push(TokenUsageRow {
            label: i18n("agent-context-cache-write"),
            tokens,
            nested: true,
        });
    }
    if let Some(tokens) = usage.output_tokens {
        rows.push(TokenUsageRow {
            label: i18n("agent-context-output"),
            tokens,
            nested: false,
        });
    }
    if let Some(tokens) = usage.reasoning_output_tokens {
        rows.push(TokenUsageRow {
            label: i18n("agent-context-reasoning"),
            tokens,
            nested: true,
        });
    }

    rows
}

fn cumulative_usage_heading(scope: ContextUsageScope) -> &'static str {
    match scope {
        ContextUsageScope::Thread => i18n("agent-context-thread-total"),
        ContextUsageScope::LastTurn => i18n("agent-context-last-turn"),
    }
}

impl RenderOnce for TokenUsageRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;

        h_flex()
            .w_full()
            .justify_between()
            .gap_3()
            .when(self.nested, |this| this.pl_3())
            .child(
                div()
                    .text_color(if self.nested {
                        muted.opacity(0.72)
                    } else {
                        foreground.opacity(0.86)
                    })
                    .child(self.label),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(if self.nested {
                        muted.opacity(0.72)
                    } else {
                        foreground.opacity(0.86)
                    })
                    .child(compact_token_count(self.tokens)),
            )
    }
}

impl RenderOnce for ContextUsageIndicator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let usage = self.usage;
        let indicator_label = context_indicator_label(usage);
        let accessibility_label =
            i18n("agent-context-accessibility").replace("{usage}", &indicator_label);
        let (capacity_label, remaining_label) = context_capacity_labels(usage);
        // Both sections report the same categories, so the live context and
        // the last turn can be read against each other. A conversation
        // restored from history knows only its total, and that alone keeps the
        // section present until the first reply reports the categories.
        let current_rows = token_usage_rows(usage.current, /*include_total*/ true);
        let cumulative = usage.cumulative;
        let segment_rows = self
            .composition
            .as_ref()
            .map(context_segment_rows)
            .unwrap_or_default();
        // A conversation that has not closed a step yet reports zeroes, which
        // describe nothing; the section appears once there is something in it.
        let stats = self.stats.filter(|stats| stats.steps > 0);

        let trigger = h_flex()
            .id("agent-context-trigger")
            .flex_none()
            .gap_1p5()
            .items_center()
            .aria_label(accessibility_label)
            .text_color(cx.theme().muted_foreground.opacity(0.72))
            .child(Icon::new(IconName::ChartPie).size_3())
            .child(div().child(indicator_label));

        HoverCard::new("agent-context-usage")
            .anchor(gpui::Anchor::BottomRight)
            .open_delay(Duration::from_millis(250))
            .close_delay(Duration::from_millis(150))
            .trigger(trigger)
            .content(move |_, _, cx| {
                let foreground = cx.theme().foreground;
                let muted = cx.theme().muted_foreground;

                v_flex()
                    .w(px(248.))
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(muted.opacity(0.72))
                                    .child(i18n("agent-context-heading")),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(foreground)
                                            .child(capacity_label.clone()),
                                    )
                                    .when_some(remaining_label.clone(), |this, label| {
                                        this.child(
                                            div()
                                                .flex_none()
                                                .text_xs()
                                                .text_color(cx.theme().primary)
                                                .child(label),
                                        )
                                    }),
                            ),
                    )
                    .when(!segment_rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .pt_2()
                                .border_t_1()
                                .border_color(cx.theme().border.opacity(0.6))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(muted.opacity(0.72))
                                        .child(i18n("agent-context-what-fills-it")),
                                )
                                .children(segment_rows.iter().cloned()),
                        )
                    })
                    .when(!current_rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .pt_2()
                                .border_t_1()
                                .border_color(cx.theme().border.opacity(0.6))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(muted.opacity(0.72))
                                        .child(i18n("agent-context-current")),
                                )
                                .children(current_rows.iter().copied()),
                        )
                    })
                    .when_some(stats, |this, stats| {
                        let rows = [
                            ("agent-context-session-turns", stats.turns.to_string()),
                            ("agent-context-session-steps", stats.steps.to_string()),
                            (
                                "agent-context-session-model-time",
                                wall_time_readout(stats.model_ms),
                            ),
                            (
                                "agent-context-session-tool-time",
                                wall_time_readout(stats.tool_ms),
                            ),
                        ];

                        this.child(
                            v_flex()
                                .gap_1()
                                .pt_2()
                                .border_t_1()
                                .border_color(cx.theme().border.opacity(0.6))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(muted.opacity(0.72))
                                        .child(i18n("agent-context-session-heading")),
                                )
                                .children(rows.map(|(label, value)| {
                                    h_flex()
                                        .w_full()
                                        .justify_between()
                                        .gap_3()
                                        .text_xs()
                                        .child(
                                            div()
                                                .text_color(muted.opacity(0.86))
                                                .child(i18n(label)),
                                        )
                                        .child(
                                            div().text_color(foreground.opacity(0.86)).child(value),
                                        )
                                })),
                        )
                    })
                    .when_some(cumulative, |this, cumulative| {
                        let rows = token_usage_rows(cumulative.breakdown, true);

                        this.child(
                            v_flex()
                                .gap_1()
                                .pt_2()
                                .border_t_1()
                                .border_color(cx.theme().border.opacity(0.6))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(muted.opacity(0.72))
                                        .child(cumulative_usage_heading(cumulative.scope)),
                                )
                                .children(rows),
                        )
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use nmt_agent_utils::chat::ScopedTokenUsage;

    use super::*;

    fn context_usage(used_tokens: u64, max_tokens: Option<u64>) -> ContextWindowUsage {
        ContextWindowUsage {
            current: TokenUsageBreakdown::total_only(used_tokens),
            cumulative: None,
            max_tokens,
        }
    }

    fn composition(segments: &[(&str, u64, bool)], max_tokens: Option<u64>) -> ContextComposition {
        ContextComposition {
            segments: segments
                .iter()
                .map(|(label, tokens, deferred)| ContextSegment {
                    label: (*label).to_string(),
                    tokens: *tokens,
                    color: None,
                    deferred: *deferred,
                })
                .collect(),
            used_tokens: segments.iter().map(|(_, tokens, _)| tokens).sum(),
            max_tokens,
            raw_max_tokens: None,
            auto_compact_threshold: None,
        }
    }

    #[test]
    fn the_cache_share_is_measured_over_the_turn_not_its_last_request() {
        // The shape a tool loop produces: the turn wrote 30k tokens into the
        // cache up front, while its final request replayed an almost entirely
        // cached prefix.
        let last_request = TokenUsageBreakdown {
            total_tokens: 101_000,
            input_tokens: Some(100_000),
            cache_read_input_tokens: Some(99_800),
            cache_write_input_tokens: Some(200),
            output_tokens: Some(1_000),
            reasoning_output_tokens: None,
        };
        let turn = TokenUsageBreakdown {
            total_tokens: 310_000,
            input_tokens: Some(300_000),
            cache_read_input_tokens: Some(270_000),
            cache_write_input_tokens: Some(30_000),
            output_tokens: Some(10_000),
            reasoning_output_tokens: None,
        };

        let usage = ContextWindowUsage {
            current: last_request,
            cumulative: Some(ScopedTokenUsage {
                scope: ContextUsageScope::LastTurn,
                breakdown: turn,
            }),
            max_tokens: Some(200_000),
        };
        assert_eq!(cache_hit_percent(usage), Some(90));

        // Without an aggregate — a sparse or post-compaction snapshot — the
        // newest request is still worth reporting.
        assert_eq!(
            cache_hit_percent(ContextWindowUsage {
                cumulative: None,
                ..usage
            }),
            Some(100)
        );
        assert_eq!(
            cache_hit_percent(ContextWindowUsage {
                cumulative: Some(ScopedTokenUsage {
                    scope: ContextUsageScope::Thread,
                    breakdown: TokenUsageBreakdown::total_only(500_000),
                }),
                ..usage
            }),
            Some(100)
        );
    }

    #[test]
    fn the_live_context_and_the_last_turn_report_the_same_categories() {
        let usage = TokenUsageBreakdown {
            total_tokens: 12_345,
            input_tokens: Some(10_000),
            cache_read_input_tokens: Some(2_000),
            cache_write_input_tokens: Some(500),
            output_tokens: Some(345),
            reasoning_output_tokens: None,
        };

        // Both sections are built the same way, so the two can be read against
        // each other rather than one omitting a figure the other shows.
        let labels: Vec<_> = token_usage_rows(usage, true)
            .iter()
            .map(|row| row.label)
            .collect();
        assert_eq!(
            labels,
            ["Total", "Input", "Cache read", "Cache write", "Output"]
        );
    }

    #[test]
    fn a_restored_conversation_still_reports_its_context_size() {
        // Restored from a context breakdown: the window size is known, the
        // billing categories are not, because the breakdown reports what fills
        // the window rather than how tokens were billed.
        let rows = token_usage_rows(TokenUsageBreakdown::total_only(41_000), true);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Total");
        assert_eq!(rows[0].tokens, 41_000);
    }

    #[test]
    fn segments_lead_with_what_fills_the_window() {
        let rows = context_segment_rows(&composition(
            &[
                ("System prompt", 3_000, false),
                ("Messages", 40_000, false),
                ("Tools", 7_000, false),
            ],
            Some(100_000),
        ));

        let labels: Vec<_> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["Messages", "Tools", "System prompt"]);
        // Shares are taken against the measured window, so they agree with the
        // capacity line the card shows above them.
        assert_eq!(rows[0].percent, 40);
        assert_eq!(rows[1].percent, 7);
    }

    #[test]
    fn empty_segments_are_dropped_and_deferred_ones_are_marked() {
        let rows = context_segment_rows(&composition(
            &[
                ("Free space", 0, false),
                ("Messages", 1_000, false),
                ("Reserved", 500, true),
            ],
            Some(10_000),
        ));

        assert_eq!(rows.len(), 2, "a part occupying nothing is not a row");
        assert!(!rows[0].deferred);
        assert!(rows[1].deferred);
    }

    #[test]
    fn shares_fall_back_to_the_measured_total_without_a_window() {
        let rows = context_segment_rows(&composition(
            &[("Messages", 750, false), ("Tools", 250, false)],
            None,
        ));

        assert_eq!(rows[0].percent, 75);
        assert_eq!(rows[1].percent, 25);
    }

    #[test]
    fn context_capacity_formats_known_and_unknown_limits() {
        let known = context_usage(41_000, Some(258_400));
        assert_eq!(context_indicator_label(known), "41k used · 84% left");
        assert_eq!(
            context_capacity_labels(known),
            ("41k / 258k".to_string(), Some("84% left".to_string()))
        );

        let unknown = context_usage(9_000, None);
        assert_eq!(context_indicator_label(unknown), "9k used");
        assert_eq!(
            context_capacity_labels(unknown),
            ("9k used".to_string(), None)
        );
    }

    #[test]
    fn token_rows_keep_available_categories_and_their_hierarchy() {
        let rows = token_usage_rows(
            TokenUsageBreakdown {
                total_tokens: 16_700,
                input_tokens: Some(15_500),
                cache_read_input_tokens: Some(2_000),
                cache_write_input_tokens: Some(5_000),
                output_tokens: Some(1_200),
                reasoning_output_tokens: None,
            },
            false,
        );

        assert_eq!(
            rows,
            vec![
                TokenUsageRow {
                    label: "Input",
                    tokens: 15_500,
                    nested: false,
                },
                TokenUsageRow {
                    label: "Cache read",
                    tokens: 2_000,
                    nested: true,
                },
                TokenUsageRow {
                    label: "Cache write",
                    tokens: 5_000,
                    nested: true,
                },
                TokenUsageRow {
                    label: "Output",
                    tokens: 1_200,
                    nested: false,
                },
            ]
        );
        assert_eq!(
            token_usage_rows(TokenUsageBreakdown::total_only(17_000), false),
            Vec::new()
        );
    }

    #[test]
    fn cumulative_headings_name_the_provider_scope() {
        assert_eq!(
            cumulative_usage_heading(ContextUsageScope::Thread),
            "Thread total"
        );
        assert_eq!(
            cumulative_usage_heading(ContextUsageScope::LastTurn),
            "Last turn"
        );
    }
}
