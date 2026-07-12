//! Titlebar widget showing today's Claude token usage from `ccusage`.
//!
//! Runs `npx ccusage@latest -j --offline --since <today>` on the background
//! executor (hidden console via `CREATE_NO_WINDOW`), parses the JSON `totals`,
//! and renders them as `i:<input> o:<output> cw:<cache_write> cr:<cache_read>`.
//! Auto-refreshes every 60 seconds while the Appearance toggle is on; clicking
//! the widget refreshes immediately.

use std::os::windows::process::CommandExt as _;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, SharedString, Window};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::ui::AppSettings;

/// Shown before the first successful fetch (and kept on fetch errors).
const PLACEHOLDER: &str = "i:- o:- cw:- cr:-";

pub(crate) struct TokenUsageView {
    text: SharedString,
    refreshing: bool,
    /// Mirror of `AppSettings::show_daily_token_usage`, tracked so the global
    /// observer refreshes only on the off→on edge (settings fire for every
    /// change, e.g. font size).
    enabled: bool,
}

impl TokenUsageView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let enabled = cx.global::<AppSettings>().show_daily_token_usage;
        cx.observe_global::<AppSettings>(|this, cx| {
            let enabled = cx.global::<AppSettings>().show_daily_token_usage;
            if enabled && !this.enabled {
                this.refresh(cx);
            }
            this.enabled = enabled;
        })
        .detach();
        // 60-second auto-refresh, idle while the toggle is off. Lives as long
        // as the view entity (the loop exits when the entity is dropped).
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(60))
                    .await;
                let alive = this.update(cx, |this, cx| {
                    if this.enabled {
                        this.refresh(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut this = Self {
            text: PLACEHOLDER.into(),
            refreshing: false,
            enabled,
        };
        if enabled {
            this.refresh(cx);
        }
        this
    }

    /// Kick off one fetch on the background executor; no-op while one is
    /// already in flight. Errors keep the previous text and log a warning.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        cx.notify();
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        let fetch = cx
            .background_executor()
            .spawn(async move { fetch_usage(&today) });
        cx.spawn(async move |this, _cx| {
            let result = fetch.await;
            this.update(_cx, |this, cx| {
                this.refreshing = false;
                match result {
                    Ok(text) => this.text = text.into(),
                    Err(err) => tracing::warn!("token usage refresh failed: {err}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for TokenUsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("token-usage")
            .ghost()
            .small()
            .label(self.text.clone())
            .loading(self.refreshing)
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
    }
}

/// Run ccusage for `today` (yyyymmdd) and format its stdout JSON.
fn fetch_usage(today: &str) -> Result<String, String> {
    // `npx` is a `.cmd` shim on Windows, so it must be launched through
    // cmd.exe; `CREATE_NO_WINDOW` keeps the run silent (no console flash).
    let output = std::process::Command::new("cmd")
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
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("ccusage output is not valid JSON: {err}"))?;
    Ok(format_usage(&json))
}

/// `i:<input> o:<output> cw:<cache_write> cr:<cache_read>` from the report's
/// `totals` object; missing fields read as 0.
fn format_usage(json: &serde_json::Value) -> String {
    let totals = &json["totals"];
    let field = |key: &str| totals[key].as_u64().unwrap_or(0);
    format!(
        "i:{} o:{} cw:{} cr:{}",
        compact(field("inputTokens")),
        compact(field("outputTokens")),
        compact(field("cacheCreationTokens")),
        compact(field("cacheReadTokens")),
    )
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
    use super::*;

    #[test]
    fn format_usage_reads_totals() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "daily": [{"date": "2026-07-03"}],
                "totals": {
                    "inputTokens": 1234,
                    "outputTokens": 56789,
                    "cacheCreationTokens": 2500000,
                    "cacheReadTokens": 1200000000
                }
            }"#,
        )
        .unwrap();
        assert_eq!(format_usage(&json), "i:1234 o:56.8k cw:2.5M cr:1.20B");
    }

    #[test]
    fn format_usage_defaults_missing_fields_to_zero() {
        let json: serde_json::Value = serde_json::from_str("{}").unwrap();
        assert_eq!(format_usage(&json), "i:0 o:0 cw:0 cr:0");
    }
}
