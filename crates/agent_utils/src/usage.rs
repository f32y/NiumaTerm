//! Provider-neutral subscription-limit data used by compact usage surfaces.

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use serde_json::Value;

pub const FIVE_HOUR_WINDOW_MINUTES: u32 = 5 * 60;
pub const WEEKLY_WINDOW_MINUTES: u32 = 7 * 24 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageWindow {
    pub remaining_percentage: u8,
    pub window_minutes: u32,
    pub resets_at: Option<i64>,
    pub reset_description: Option<String>,
}

impl UsageWindow {
    pub fn new(remaining_percentage: u8, window_minutes: u32) -> Self {
        Self {
            remaining_percentage,
            window_minutes,
            resets_at: None,
            reset_description: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageResetCredits {
    pub available_count: u64,
    pub next_expires_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageSnapshot {
    pub five_hour: Option<UsageWindow>,
    pub weekly: Option<UsageWindow>,
    pub fable_weekly: Option<UsageWindow>,
    pub plan_type: Option<String>,
    pub reset_credits: Option<UsageResetCredits>,
    pub updated_at: Option<i64>,
}

impl UsageSnapshot {
    pub fn is_unavailable(&self) -> bool {
        self.five_hour.is_none() && self.weekly.is_none() && self.fable_weekly.is_none()
    }

    pub fn compact_values(&self) -> [String; 2] {
        [
            format_remaining(self.five_hour.as_ref()),
            format_remaining(self.weekly.as_ref()),
        ]
    }

    pub fn with_updated_now(mut self) -> Self {
        self.updated_at = Some(now_unix_millis());
        self
    }

    /// Fill windows this snapshot is missing from one taken through another
    /// source. Sources disagree about which windows they report at all rather
    /// than about their values, so a window already present is never replaced:
    /// two readings of the same window differ only by the seconds between
    /// them, and the first one asked is the more authoritative source.
    pub fn filled_from(mut self, other: &Self) -> Self {
        self.five_hour = self.five_hour.or_else(|| other.five_hour.clone());
        self.weekly = self.weekly.or_else(|| other.weekly.clone());
        self.fable_weekly = self.fable_weekly.or_else(|| other.fable_weekly.clone());
        self.plan_type = self.plan_type.or_else(|| other.plan_type.clone());
        self.reset_credits = self.reset_credits.or_else(|| other.reset_credits.clone());
        self
    }
}

pub fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(i64::MAX as u128) as i64
        })
}

pub(crate) fn parse_timestamp_millis(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_f64().filter(|number| number.is_finite()) {
        let millis = if number.abs() < 10_000_000_000.0 {
            number * 1_000.0
        } else {
            number
        };
        return (millis >= i64::MIN as f64 && millis <= i64::MAX as f64)
            .then_some(millis.round() as i64);
    }

    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(number) = text.parse::<f64>() {
        return parse_timestamp_millis(&Value::from(number));
    }

    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

pub fn format_remaining(window: Option<&UsageWindow>) -> String {
    window.map_or_else(
        || "—".to_string(),
        |window| format!("{}%", window.remaining_percentage),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn filling_adds_missing_windows_and_keeps_the_ones_already_read() {
        let panel = super::UsageSnapshot {
            five_hour: Some(super::UsageWindow::new(10, 300)),
            weekly: Some(super::UsageWindow::new(20, 10_080)),
            fable_weekly: Some(super::UsageWindow::new(30, 10_080)),
            ..super::UsageSnapshot::default()
        };
        let endpoint = super::UsageSnapshot {
            five_hour: Some(super::UsageWindow::new(88, 300)),
            ..super::UsageSnapshot::default()
        };

        let filled = endpoint.filled_from(&panel);

        // The window the endpoint read stands; the ones it left out arrive.
        assert_eq!(filled.five_hour, Some(super::UsageWindow::new(88, 300)));
        assert_eq!(filled.weekly, panel.weekly);
        assert_eq!(filled.fable_weekly, panel.fable_weekly);
    }

    use super::*;

    #[test]
    fn projects_available_and_missing_windows_for_compact_display() {
        assert_eq!(
            UsageSnapshot {
                five_hour: Some(UsageWindow::new(97, FIVE_HOUR_WINDOW_MINUTES)),
                ..UsageSnapshot::default()
            }
            .compact_values(),
            ["97%", "—"]
        );
    }

    #[test]
    fn provider_metadata_does_not_count_as_a_quota_window() {
        assert!(
            UsageSnapshot {
                plan_type: Some("plus".to_string()),
                ..UsageSnapshot::default()
            }
            .is_unavailable()
        );
    }
}
