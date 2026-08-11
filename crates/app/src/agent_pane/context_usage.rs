use std::time::Duration;

use gpui::prelude::*;
use gpui::{App, FontWeight, IntoElement, RenderOnce, Window, div, px};
use gpui_component::hover_card::HoverCard;
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};
use nmt_agent_utils::chat::{
    ContextComposition, ContextSegment, ContextUsageScope, ContextWindowUsage, TokenUsageBreakdown,
};

use super::transcript::compact_token_count;

#[derive(IntoElement)]
pub(super) struct ContextUsageIndicator {
    usage: ContextWindowUsage,
    /// What fills the window, when the provider measures it. Codex reports
    /// only accounting, so its card shows the accounting alone.
    composition: Option<ContextComposition>,
}

impl ContextUsageIndicator {
    pub(super) fn new(usage: ContextWindowUsage, composition: Option<ContextComposition>) -> Self {
        Self { usage, composition }
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
                    .child(if self.deferred {
                        format!("{} (deferred)", self.label)
                    } else {
                        self.label
                    }),
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

fn context_indicator_label(usage: ContextWindowUsage) -> String {
    match remaining_context_percent(usage) {
        Some(remaining_percent) => format!(
            "{} used · {remaining_percent}% left",
            compact_token_count(usage.used_tokens())
        ),
        None => format!("{} used", compact_token_count(usage.used_tokens())),
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
            remaining_context_percent(usage).map(|percent| format!("{percent}% left")),
        ),
        None => (
            format!("{} used", compact_token_count(usage.used_tokens())),
            None,
        ),
    }
}

fn token_usage_rows(usage: TokenUsageBreakdown, include_total: bool) -> Vec<TokenUsageRow> {
    let mut rows = Vec::new();

    if include_total {
        rows.push(TokenUsageRow {
            label: "Total",
            tokens: usage.total_tokens,
            nested: false,
        });
    }
    if let Some(tokens) = usage.input_tokens {
        rows.push(TokenUsageRow {
            label: "Input",
            tokens,
            nested: false,
        });
    }
    if let Some(tokens) = usage.cache_read_input_tokens {
        rows.push(TokenUsageRow {
            label: "Cache read",
            tokens,
            nested: true,
        });
    }
    if let Some(tokens) = usage.cache_write_input_tokens {
        rows.push(TokenUsageRow {
            label: "Cache write",
            tokens,
            nested: true,
        });
    }
    if let Some(tokens) = usage.output_tokens {
        rows.push(TokenUsageRow {
            label: "Output",
            tokens,
            nested: false,
        });
    }
    if let Some(tokens) = usage.reasoning_output_tokens {
        rows.push(TokenUsageRow {
            label: "Reasoning",
            tokens,
            nested: true,
        });
    }

    rows
}

fn cumulative_usage_heading(scope: ContextUsageScope) -> &'static str {
    match scope {
        ContextUsageScope::Thread => "Thread total",
        ContextUsageScope::LastTurn => "Last turn",
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
        let accessibility_label = format!("Agent context: {indicator_label}");
        let (capacity_label, remaining_label) = context_capacity_labels(usage);
        let current_rows = token_usage_rows(usage.current, false);
        let cumulative = usage.cumulative;
        let segment_rows = self
            .composition
            .as_ref()
            .map(context_segment_rows)
            .unwrap_or_default();

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
                                    .child("AGENT CONTEXT"),
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
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(muted.opacity(0.72))
                                        .child("What fills it"),
                                )
                                .children(segment_rows.iter().cloned()),
                        )
                    })
                    .when(!current_rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(muted.opacity(0.72))
                                        .child("Current context"),
                                )
                                .children(current_rows.iter().copied()),
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
