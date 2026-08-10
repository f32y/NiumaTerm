use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, Hsla, SharedString, Window, div, px, relative};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::hover_card::HoverCard;
use gpui_component::{ActiveTheme as _, Icon, IconNamed, Sizable as _, h_flex, v_flex};
use nmt_agent_utils::claude_code::usage_fetcher as claude_usage;
use nmt_agent_utils::codex::usage_fetcher as codex_usage;
use nmt_agent_utils::usage::{UsageSnapshot, UsageWindow, now_unix_millis};
use tracing::warn;

use crate::ui::AppSettings;

const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

pub(crate) struct CodexIcon;

impl IconNamed for CodexIcon {
    fn path(self) -> SharedString {
        "icons/codex.svg".into()
    }
}

pub(crate) struct ClaudeIcon;

impl IconNamed for ClaudeIcon {
    fn path(self) -> SharedString {
        "icons/claude.svg".into()
    }
}

#[derive(Default)]
struct ProviderRefresh {
    refreshing: bool,
    failed: bool,
    cancel: Arc<AtomicBool>,
}

pub(crate) struct AgentUsageView {
    codex: UsageSnapshot,
    claude: UsageSnapshot,
    codex_refresh: ProviderRefresh,
    claude_refresh: ProviderRefresh,
    enabled: bool,
}

impl Drop for AgentUsageView {
    fn drop(&mut self) {
        self.codex_refresh.cancel.store(true, Ordering::Relaxed);
        self.claude_refresh.cancel.store(true, Ordering::Relaxed);
    }
}

impl AgentUsageView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let enabled = cx.global::<AppSettings>().show_agent_usage;
        let mut this = Self {
            codex: UsageSnapshot::default(),
            claude: UsageSnapshot::default(),
            codex_refresh: ProviderRefresh::default(),
            claude_refresh: ProviderRefresh::default(),
            enabled,
        };

        cx.observe_global::<AppSettings>(|this: &mut Self, cx| {
            let enabled = cx.global::<AppSettings>().show_agent_usage;
            if enabled && !this.enabled {
                this.enabled = true;
                this.refresh_all(cx);
            } else if !enabled && this.enabled {
                this.enabled = false;
                this.codex_refresh.cancel.store(true, Ordering::Relaxed);
                this.claude_refresh.cancel.store(true, Ordering::Relaxed);
            }
        })
        .detach();

        cx.spawn(
            async move |view: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| loop {
                cx.background_executor().timer(REFRESH_INTERVAL).await;
                if view
                    .update(cx, |this, cx| {
                        if this.enabled {
                            this.refresh_all(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            },
        )
        .detach();

        if enabled {
            this.refresh_all(cx);
        }

        this
    }

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.refresh_codex(cx);
        self.refresh_claude(cx);
    }

    fn refresh_codex(&mut self, cx: &mut Context<Self>) {
        if self.codex_refresh.refreshing {
            return;
        }

        self.codex_refresh.refreshing = true;
        self.codex_refresh.cancel.store(true, Ordering::Relaxed);
        self.codex_refresh.cancel = Arc::new(AtomicBool::new(false));
        cx.notify();

        let fetch = cx
            .background_executor()
            .spawn(async move { codex_usage::fetch() });
        cx.spawn(
            async move |view: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let output = fetch.await;
                view.update(cx, |this, cx| {
                    this.codex_refresh.refreshing = false;
                    match output {
                        Ok(usage) => {
                            this.codex = usage;
                            this.codex_refresh.failed = false;
                        }
                        Err(err) => {
                            this.codex_refresh.failed = true;
                            warn!("Codex usage refresh failed: {err}");
                        }
                    }
                    cx.notify();
                })
                .ok();
            },
        )
        .detach();
    }

    fn refresh_claude(&mut self, cx: &mut Context<Self>) {
        if self.claude_refresh.refreshing {
            return;
        }

        self.claude_refresh.refreshing = true;
        self.claude_refresh.cancel.store(true, Ordering::Relaxed);
        self.claude_refresh.cancel = Arc::new(AtomicBool::new(false));
        let cancelled = self.claude_refresh.cancel.clone();
        cx.notify();

        let fetch = cx
            .background_executor()
            .spawn(async move { claude_usage::fetch_with_cancel(&cancelled) });
        cx.spawn(
            async move |view: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let output = fetch.await;
                view.update(cx, |this, cx| {
                    this.claude_refresh.refreshing = false;
                    match output {
                        Ok(usage) => {
                            this.claude = usage;
                            this.claude_refresh.failed = false;
                        }
                        Err(err) => {
                            let retry = this.enabled && err == "Claude usage request cancelled";
                            this.claude_refresh.failed = true;
                            warn!("Claude usage refresh failed: {err}");
                            if retry {
                                this.refresh_claude(cx);
                            }
                        }
                    }
                    cx.notify();
                })
                .ok();
            },
        )
        .detach();
    }

    fn accessibility_label(&self) -> String {
        let [codex_five_hour, codex_week] = self.codex.compact_values();
        let [claude_five_hour, claude_week] = self.claude.compact_values();
        let refreshing = (self.codex_refresh.refreshing || self.claude_refresh.refreshing)
            .then_some(" Refreshing.")
            .unwrap_or_default();

        format!(
            "Agent usage remaining. Codex five hour: {codex_five_hour}; Codex week: {codex_week}; Claude five hour: {claude_five_hour}; Claude week: {claude_week}.{refreshing}"
        )
    }
}

#[derive(Clone, Copy)]
struct UsagePanelColors {
    foreground: Hsla,
    muted: Hsla,
    border: Hsla,
    track: Hsla,
    normal: Hsla,
    warning: Hsla,
    danger: Hsla,
}

struct UsageWindowRow<'a> {
    label: &'static str,
    window: &'a UsageWindow,
}

fn usage_window_rows(usage: &UsageSnapshot) -> Vec<UsageWindowRow<'_>> {
    let mut rows = Vec::with_capacity(3);
    if let Some(window) = usage.five_hour.as_ref() {
        rows.push(UsageWindowRow {
            label: "Session",
            window,
        });
    }
    if let Some(window) = usage.weekly.as_ref() {
        rows.push(UsageWindowRow {
            label: "Weekly",
            window,
        });
    }
    if let Some(window) = usage.fable_weekly.as_ref() {
        rows.push(UsageWindowRow {
            label: "Fable weekly",
            window,
        });
    }
    rows
}

fn format_window_duration(window_minutes: u32) -> String {
    if window_minutes % (24 * 60) == 0 {
        format!("{}d", window_minutes / (24 * 60))
    } else if window_minutes % 60 == 0 {
        format!("{}h", window_minutes / 60)
    } else {
        format!("{window_minutes}m")
    }
}

fn format_duration_until(timestamp: i64, now: i64) -> String {
    let remaining = timestamp.saturating_sub(now);
    if remaining <= 0 {
        return "now".to_string();
    }

    let total_minutes = remaining.saturating_add(59_999) / 60_000;
    if total_minutes < 60 {
        return format!("{total_minutes}m");
    }

    let total_hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if total_hours < 24 {
        return if minutes == 0 {
            format!("{total_hours}h")
        } else {
            format!("{total_hours}h {minutes}m")
        };
    }

    let days = total_hours / 24;
    let hours = total_hours % 24;
    if hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d {hours}h")
    }
}

fn format_reset_label(window: &UsageWindow, now: i64) -> Option<String> {
    window
        .resets_at
        .map(|timestamp| match format_duration_until(timestamp, now) {
            duration if duration == "now" => "Resets now".to_string(),
            duration => format!("Resets in {duration}"),
        })
        .or_else(|| window.reset_description.clone())
}

fn format_updated_label(usage: &UsageSnapshot, refreshing: bool, failed: bool, now: i64) -> String {
    if refreshing {
        return "Refreshing…".to_string();
    }
    if failed && usage.updated_at.is_none() {
        return "Usage unavailable".to_string();
    }

    let Some(updated_at) = usage.updated_at else {
        return "Waiting for usage data".to_string();
    };
    let elapsed = now.saturating_sub(updated_at);
    let age = if elapsed < 60_000 {
        "just now".to_string()
    } else if elapsed < 60 * 60_000 {
        format!("{}m ago", elapsed / 60_000)
    } else {
        format!("{}h ago", elapsed / (60 * 60_000))
    };

    if failed {
        format!("Refresh failed · updated {age}")
    } else {
        format!("Updated {age}")
    }
}

fn reset_credit_label(usage: &UsageSnapshot, now: i64) -> Option<String> {
    let credits = usage.reset_credits.as_ref()?;
    let count_label = match credits.available_count {
        1 => "1 limit reset available".to_string(),
        count => format!("{count} limit resets available"),
    };
    Some(match credits.next_expires_at {
        Some(expires_at) => match format_duration_until(expires_at, now) {
            duration if duration == "now" => format!("{count_label} · next expires now"),
            duration => format!("{count_label} · next expires in {duration}"),
        },
        None => count_label,
    })
}

fn usage_bar_color(remaining_percentage: u8, colors: UsagePanelColors) -> Hsla {
    if remaining_percentage > 40 {
        colors.normal
    } else if remaining_percentage > 20 {
        colors.warning
    } else {
        colors.danger
    }
}

fn render_usage_window(row: UsageWindowRow<'_>, now: i64, colors: UsagePanelColors) -> AnyElement {
    let remaining = row.window.remaining_percentage;
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.foreground)
                        .child(format!(
                            "{} ({})",
                            row.label,
                            format_window_duration(row.window.window_minutes)
                        )),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(colors.muted)
                        .child(format!("{remaining}% left")),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(6.))
                .rounded_full()
                .overflow_hidden()
                .bg(colors.track)
                .child(
                    div()
                        .h_full()
                        .w(relative(f32::from(remaining) / 100.0))
                        .rounded_full()
                        .bg(usage_bar_color(remaining, colors)),
                ),
        )
        .when_some(format_reset_label(row.window, now), |this, label| {
            this.child(
                div()
                    .w_full()
                    .text_right()
                    .text_xs()
                    .text_color(colors.muted.opacity(0.78))
                    .child(label),
            )
        })
        .into_any_element()
}

fn render_provider_panel(
    name: &'static str,
    icon: AnyElement,
    usage: &UsageSnapshot,
    refreshing: bool,
    failed: bool,
    now: i64,
    colors: UsagePanelColors,
) -> AnyElement {
    let rows = usage_window_rows(usage);
    let status = format_updated_label(usage, refreshing, failed, now);
    let plan = usage.plan_type.as_ref().map(|plan| format!("{plan} plan"));

    v_flex()
        .w_full()
        .gap_2()
        .child(
            v_flex()
                .gap_0p5()
                .child(
                    h_flex().items_center().gap_1p5().child(icon).child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.foreground)
                            .child(name),
                    ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.muted.opacity(0.78))
                        .child(status),
                )
                .when_some(plan, |this, plan| {
                    this.child(div().text_xs().text_color(colors.muted).child(plan))
                })
                .when_some(reset_credit_label(usage, now), |this, label| {
                    this.child(div().text_xs().text_color(colors.muted).child(label))
                }),
        )
        .when(rows.is_empty(), |this| {
            this.child(
                div()
                    .py_1()
                    .text_xs()
                    .text_color(colors.muted)
                    .child("No subscription limits available"),
            )
        })
        .children(
            rows.into_iter()
                .map(|row| render_usage_window(row, now, colors)),
        )
        .into_any_element()
}

impl Render for AgentUsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let [codex_five_hour, codex_week] = self.codex.compact_values();
        let [claude_five_hour, claude_week] = self.claude.compact_values();
        let refreshing = self.codex_refresh.refreshing || self.claude_refresh.refreshing;
        let codex = self.codex.clone();
        let claude = self.claude.clone();
        let codex_refreshing = self.codex_refresh.refreshing;
        let claude_refreshing = self.claude_refresh.refreshing;
        let codex_failed = self.codex_refresh.failed;
        let claude_failed = self.claude_refresh.failed;

        let trigger = Button::new("agent-usage")
            .ghost()
            .small()
            .w_full()
            .h(px(28.))
            .px_1()
            .aria_label(self.accessibility_label())
            // Opacity communicates in-flight work without replacing or moving
            // the last successful values in this tightly packed status line.
            .when(refreshing, |this| this.opacity(0.65))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .justify_center()
                    .gap_1()
                    .text_xs()
                    .child(
                        div()
                            .id("agent-usage-codex-icon")
                            .aria_label("Codex")
                            .child(Icon::new(CodexIcon).xsmall()),
                    )
                    .child(codex_five_hour)
                    .child(codex_week)
                    // The outer one-space gap plus these margins yields the
                    // required two-space separation on each side of `|`.
                    .child(div().mx_1().child("|"))
                    .child(
                        div()
                            .id("agent-usage-claude-icon")
                            .aria_label("Claude")
                            .child(Icon::new(ClaudeIcon).xsmall()),
                    )
                    .child(claude_five_hour)
                    .child(claude_week),
            )
            .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx)));

        div().w_full().child(
            HoverCard::new("agent-usage-details")
                .anchor(gpui::Anchor::BottomLeft)
                .open_delay(Duration::from_millis(250))
                .close_delay(Duration::from_millis(150))
                .trigger(trigger)
                .content(move |_, _, cx| {
                    let colors = UsagePanelColors {
                        foreground: cx.theme().foreground,
                        muted: cx.theme().muted_foreground,
                        border: cx.theme().border,
                        track: cx.theme().muted.opacity(0.65),
                        normal: cx.theme().primary,
                        warning: cx.theme().warning,
                        danger: cx.theme().danger,
                    };
                    let now = now_unix_millis();

                    v_flex()
                        .w(px(272.))
                        .gap_3()
                        .child(render_provider_panel(
                            "Codex",
                            Icon::new(CodexIcon).small().into_any_element(),
                            &codex,
                            codex_refreshing,
                            codex_failed,
                            now,
                            colors,
                        ))
                        .child(
                            div()
                                .w_full()
                                .border_t_1()
                                .border_color(colors.border.opacity(0.65)),
                        )
                        .child(render_provider_panel(
                            "Claude",
                            Icon::new(ClaudeIcon).small().into_any_element(),
                            &claude,
                            claude_refreshing,
                            claude_failed,
                            now,
                            colors,
                        ))
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_projection_keeps_provider_and_window_order() {
        let view = AgentUsageView {
            codex: UsageSnapshot {
                five_hour: Some(UsageWindow::new(25, 300)),
                weekly: Some(UsageWindow::new(80, 10_080)),
                ..UsageSnapshot::default()
            },
            claude: UsageSnapshot {
                five_hour: Some(UsageWindow::new(3, 300)),
                ..UsageSnapshot::default()
            },
            codex_refresh: ProviderRefresh::default(),
            claude_refresh: ProviderRefresh::default(),
            enabled: true,
        };

        assert_eq!(
            view.accessibility_label(),
            "Agent usage remaining. Codex five hour: 25%; Codex week: 80%; Claude five hour: 3%; Claude week: —."
        );
    }

    #[test]
    fn detail_rows_include_the_optional_fable_window() {
        let usage = UsageSnapshot {
            five_hour: Some(UsageWindow::new(75, 300)),
            weekly: Some(UsageWindow::new(55, 10_080)),
            fable_weekly: Some(UsageWindow::new(35, 10_080)),
            ..UsageSnapshot::default()
        };

        assert_eq!(
            usage_window_rows(&usage)
                .into_iter()
                .map(|row| (row.label, row.window.remaining_percentage))
                .collect::<Vec<_>>(),
            [("Session", 75), ("Weekly", 55), ("Fable weekly", 35),]
        );
        assert_eq!(format_window_duration(300), "5h");
        assert_eq!(format_window_duration(10_080), "7d");
    }

    #[test]
    fn reset_and_update_labels_use_compact_relative_time() {
        let now = 1_000_000_000;
        let mut window = UsageWindow::new(75, 300);
        window.resets_at = Some(now + 2 * 60 * 60_000 + 5 * 60_000);
        assert_eq!(
            format_reset_label(&window, now).as_deref(),
            Some("Resets in 2h 5m")
        );

        let usage = UsageSnapshot {
            updated_at: Some(now - 7 * 60_000),
            ..UsageSnapshot::default()
        };
        assert_eq!(
            format_updated_label(&usage, false, true, now),
            "Refresh failed · updated 7m ago"
        );
    }
}
