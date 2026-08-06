//! Provider-neutral remaining-quota values used by compact usage surfaces.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageSnapshot {
    pub five_hour_remaining: Option<u8>,
    pub weekly_remaining: Option<u8>,
}

impl UsageSnapshot {
    pub fn is_unavailable(self) -> bool {
        self == Self::default()
    }

    pub fn compact_values(self) -> [String; 2] {
        [
            format_remaining(self.five_hour_remaining),
            format_remaining(self.weekly_remaining),
        ]
    }
}

pub fn format_remaining(value: Option<u8>) -> String {
    value.map_or_else(|| "—".to_string(), |value| format!("{value}%"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_available_and_missing_windows_for_compact_display() {
        assert_eq!(
            UsageSnapshot {
                five_hour_remaining: Some(97),
                weekly_remaining: None,
            }
            .compact_values(),
            ["97%", "—"]
        );
    }
}
