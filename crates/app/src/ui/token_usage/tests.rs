use gpui::AssetSource as _;

use crate::ui::assets::AppAssets;
use crate::ui::token_usage::{
    TokenIcon, compact, format_token_count, model_usage_rows, parse_usage,
};

#[test]
fn token_icon_is_embedded() {
    use gpui_component::IconNamed as _;

    let path = TokenIcon.path();
    assert!(AppAssets.load(path.as_ref()).unwrap().is_some());
}

#[test]
fn parse_usage_reads_daily_totals_and_model_details() {
    let json = br#"{
        "daily": [{
            "date": "2026-08-12",
            "inputTokens": 481653,
            "outputTokens": 429113,
            "cacheCreationTokens": 1350888,
            "cacheReadTokens": 107575141,
            "totalTokens": 109836795,
            "modelBreakdowns": [{
                "modelName": "claude-opus-5",
                "inputTokens": 15409,
                "outputTokens": 161930,
                "cacheCreationTokens": 1173121,
                "cacheReadTokens": 50280148,
                "cost": 0
            }]
        }],
        "totals": { "totalTokens": 109836795 }
    }"#;

    let usage = parse_usage(json, "2026-08-12").unwrap();

    assert_eq!(usage.counts.total(), 109_836_795);
    assert_eq!(usage.counts.input_tokens, 481_653);
    assert_eq!(usage.model_breakdowns.len(), 1);
    assert_eq!(usage.model_breakdowns[0].model_name, "claude-opus-5");
    assert_eq!(usage.model_breakdowns[0].counts.total(), 51_630_608);
}

#[test]
fn parse_usage_accepts_period_as_the_daily_date() {
    let json = br#"{
        "daily": [{
            "period": "2026-08-12",
            "inputTokens": 12,
            "outputTokens": 8,
            "totalTokens": 20,
            "modelBreakdowns": []
        }],
        "totals": { "totalTokens": 20 }
    }"#;

    let usage = parse_usage(json, "2026-08-12").unwrap();

    assert_eq!(usage.date, "2026-08-12");
    assert_eq!(usage.counts.total(), 20);
}

#[test]
fn model_usage_rows_put_the_daily_total_before_models() {
    let json = br#"{
        "daily": [{
            "period": "2026-08-12",
            "inputTokens": 12,
            "outputTokens": 8,
            "totalTokens": 20,
            "modelBreakdowns": [{
                "modelName": "claude-opus-5",
                "inputTokens": 7,
                "outputTokens": 3
            }]
        }]
    }"#;
    let usage = parse_usage(json, "2026-08-12").unwrap();

    let rows = model_usage_rows(&usage);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].label, "Today total");
    assert!(rows[0].is_daily_total);
    assert_eq!(rows[0].counts.total(), 20);
    assert_eq!(rows[1].label, "claude-opus-5");
    assert!(!rows[1].is_daily_total);
    assert_eq!(rows[1].counts.total(), 10);
}

#[test]
fn parse_usage_uses_report_totals_when_day_details_are_absent() {
    let json = br#"{
        "daily": [],
        "totals": {
            "inputTokens": 12,
            "outputTokens": 8,
            "totalTokens": 20
        }
    }"#;

    let usage = parse_usage(json, "2026-08-12").unwrap();

    assert_eq!(usage.date, "2026-08-12");
    assert_eq!(usage.counts.total(), 20);
    assert!(usage.model_breakdowns.is_empty());
}

#[test]
fn parse_usage_rejects_invalid_json() {
    let error = parse_usage(b"not json", "2026-08-12").unwrap_err();
    assert!(error.starts_with("ccusage output is not valid JSON:"));
}

#[test]
fn format_token_count_keeps_exact_values_readable() {
    assert_eq!(format_token_count(0), "0");
    assert_eq!(format_token_count(999), "999");
    assert_eq!(format_token_count(1_000), "1,000");
    assert_eq!(format_token_count(109_836_795), "109,836,795");
}

#[test]
fn compact_keeps_the_titlebar_narrow() {
    assert_eq!(compact(9_999), "9999");
    assert_eq!(compact(12_345), "12.3k");
    assert_eq!(compact(3_758_023), "3.8M");
    assert_eq!(compact(1_200_000_000), "1.20B");
}
