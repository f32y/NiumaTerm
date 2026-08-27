use nmt_agent_utils::chat::ScopedTokenUsage;

use crate::agent::context_usage::*;

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
