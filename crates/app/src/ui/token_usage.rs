//! Titlebar widget showing today's Claude token usage from `ccusage`.
//!
//! Runs `npx ccusage@latest -j --offline --since <today>` on the background
//! executor (hidden console via `CREATE_NO_WINDOW`), reads the JSON
//! `totals.totalTokens` value, and renders it beside a token icon.
//! Auto-refreshes every 60 seconds while the Appearance toggle is on; clicking
//! the widget refreshes immediately.

use std::os::windows::process::CommandExt as _;
use std::process;
use std::time::Duration;

use chrono::Local;
use gpui::prelude::*;
use gpui::{Context, SharedString, Window};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{IconNamed, Sizable as _};
use serde_json::{Value, from_slice};
use tracing::warn;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::ui::AppSettings;
use crate::ui::auto_refresh::{self, AutoRefresh, RefreshState};

/// Shown before the first successful fetch (and kept on fetch errors).
const PLACEHOLDER: &str = "-";

struct TokenIcon;

impl IconNamed for TokenIcon {
    fn path(self) -> SharedString {
        "icons/coins.svg".into()
    }
}

pub(crate) struct TokenUsageView {
    text: SharedString,
    state: RefreshState,
}

impl AutoRefresh for TokenUsageView {
    type Output = Result<String, String>;

    const INTERVAL: Duration = Duration::from_secs(60);

    fn enabled(settings: &AppSettings) -> bool {
        settings.show_daily_token_usage
    }

    fn state(&mut self) -> &mut RefreshState {
        &mut self.state
    }

    fn fetch() -> Self::Output {
        let today = Local::now().format("%Y%m%d").to_string();
        fetch_usage(&today)
    }

    fn apply(&mut self, output: Self::Output) {
        match output {
            Ok(text) => self.text = text.into(),
            Err(err) => warn!("token usage refresh failed: {err}"),
        }
    }
}

impl TokenUsageView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            text: PLACEHOLDER.into(),
            state: RefreshState {
                refreshing: false,
                enabled: Self::enabled(cx.global::<AppSettings>()),
            },
        };

        auto_refresh::start(&mut this, cx);

        this
    }
}

impl Render for TokenUsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let aria_label = format!("Daily token usage: {}", self.text);

        Button::new("token-usage")
            .ghost()
            .small()
            .icon(TokenIcon)
            .label(self.text.clone())
            .aria_label(aria_label)
            .tooltip("Daily token usage")
            .loading(self.state.refreshing)
            .on_click(cx.listener(|this, _, _, cx| auto_refresh::refresh(this, cx)))
    }
}

/// Run ccusage for `today` (yyyymmdd) and format its stdout JSON.
fn fetch_usage(today: &str) -> Result<String, String> {
    // `npx` is a `.cmd` shim on Windows, so it must be launched through
    // cmd.exe; `CREATE_NO_WINDOW` keeps the run silent (no console flash).
    let output = process::Command::new("cmd")
        .args([
            "/C",
            &format!("npx ccusage@latest -j --offline --since {today}"),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|err| format!("failed to run ccusage: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "ccusage exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let json: Value = from_slice(&output.stdout)
        .map_err(|err| format!("ccusage output is not valid JSON: {err}"))?;

    Ok(format_usage(&json))
}

/// `totalTokens` from the report's `totals` object; a missing value reads as 0.
fn format_usage(json: &Value) -> String {
    let total = json["totals"]["totalTokens"].as_u64().unwrap_or(0);
    compact(total)
}

/// Shorten large counts so the titlebar label stays narrow: 9999 stays
/// literal, then `12.3k` / `4.6M` / `1.20B`.
fn compact(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.2}B", n as f64 / 1e9),
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;
    use serde_json::from_str;

    use super::*;
    use crate::ui::assets::AppAssets;

    #[test]
    fn token_icon_is_embedded() {
        assert!(AppAssets.load("icons/coins.svg").unwrap().is_some());
    }

    #[test]
    fn format_usage_reads_total_tokens() {
        let json: Value = from_str(
            r#"{
                "daily": [{"date": "2026-07-03"}],
                "totals": {
                    "totalTokens": 3758023
                }
            }"#,
        )
        .unwrap();
        assert_eq!(format_usage(&json), "3.8M");
    }

    #[test]
    fn format_usage_defaults_missing_fields_to_zero() {
        let json: Value = from_str("{}").unwrap();
        assert_eq!(format_usage(&json), "0");
    }
}
