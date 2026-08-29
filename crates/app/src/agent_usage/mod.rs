use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, FontWeight, Hsla, Window, div, px, relative};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::hover_card::HoverCard;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use nmt_agent_utils::claude_code::usage_fetcher::{self as claude_usage, UsageFetchError};
use nmt_agent_utils::codex::usage_fetcher as codex_usage;
use nmt_agent_utils::usage::{UsageSnapshot, UsageWindow, now_unix_millis};
use nmt_app_agent::profile::{ClaudeIcon, CodexIcon};
use nmt_i18n::i18n;
use tracing::warn;

use crate::ui::AppSettings;

const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Height of the quota row, matched to the daily-total row above it so the two
/// stack as one cluster.
const QUOTA_ROW_HEIGHT: f32 = 24.0;
/// The gauge track. A bar states how much of a subscription window is left
/// without the reader having to compare two numbers, and a percentage beside
/// it keeps the exact value available.
const QUOTA_TRACK_HEIGHT: f32 = 4.0;
const QUOTA_ICON: f32 = 12.0;
const QUOTA_FILL_OPACITY: f32 = 0.7;

/// One provider half of the quota row: its mark, how much of the window it
/// reports is left, and that same figure spelled out. A provider that reports
/// no window at all gets no gauge, because an empty track would state a limit
/// nothing was measured against.
fn quota_gauge(
    id: &'static str,
    label: &'static str,
    icon: Icon,
    usage: &UsageSnapshot,
    cx: &App,
) -> Option<AnyElement> {
    let window = usage.compact_window()?;
    let value = format!("{}%", window.remaining_percentage);

    Some(
        h_flex()
            .id(id)
            .aria_label(format!("{label}: {value}"))
            .flex_1()
            .min_w_0()
            .gap_1p5()
            .items_center()
            .child(icon.with_size(px(QUOTA_ICON)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(QUOTA_TRACK_HEIGHT))
                    .rounded(px(QUOTA_TRACK_HEIGHT / 2.0))
                    .overflow_hidden()
                    .bg(cx.theme().sidebar_foreground.opacity(0.12))
                    .child(
                        div()
                            .h_full()
                            .w(relative(f32::from(window.remaining_percentage) / 100.0))
                            .rounded_full()
                            .bg(cx.theme().primary.opacity(QUOTA_FILL_OPACITY)),
                    ),
            )
            .child(div().flex_none().child(value))
            .into_any_element(),
    )
}

#[derive(Default)]
struct ProviderRefresh {
    refreshing: bool,
    failed: bool,
}

pub(crate) struct AgentUsageView {
    codex: UsageSnapshot,
    claude: UsageSnapshot,
    codex_refresh: ProviderRefresh,
    claude_refresh: ProviderRefresh,
    /// Abandons the in-flight Claude fetch. Only Claude has one: it drives an
    /// interactive CLI session for up to 25 seconds, while the Codex fetch
    /// reads local state and returns before a cancellation could reach it.
    claude_cancel: Arc<AtomicBool>,
    enabled: bool,
}

impl Drop for AgentUsageView {
    fn drop(&mut self) {
        self.claude_cancel.store(true, Ordering::Relaxed);
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
            claude_cancel: Arc::new(AtomicBool::new(false)),
            enabled,
        };

        cx.observe_global::<AppSettings>(|this: &mut Self, cx| {
            let enabled = cx.global::<AppSettings>().show_agent_usage;
            if enabled && !this.enabled {
                this.enabled = true;
                this.refresh_all(cx);
            } else if !enabled && this.enabled {
                this.enabled = false;
                this.claude_cancel.store(true, Ordering::Relaxed);
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
        self.claude_cancel.store(true, Ordering::Relaxed);
        self.claude_cancel = Arc::new(AtomicBool::new(false));
        let cancelled = self.claude_cancel.clone();
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
                        // A cancelled fetch was abandoned because the view was
                        // switched off mid-flight; if it has since been switched
                        // back on, nothing else will start the fetch it skipped.
                        Err(UsageFetchError::Cancelled) => {
                            this.claude_refresh.failed = true;
                            if this.enabled {
                                this.refresh_claude(cx);
                            }
                        }
                        Err(UsageFetchError::Failed(message)) => {
                            this.claude_refresh.failed = true;
                            warn!("Claude usage refresh failed: {message}");
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
        let refreshing = if self.codex_refresh.refreshing || self.claude_refresh.refreshing {
            i18n("agent-usage-accessibility-refreshing")
        } else {
            ""
        };

        i18n("agent-usage-accessibility")
            .replace("{codex_session}", &codex_five_hour)
            .replace("{codex_week}", &codex_week)
            .replace("{claude_session}", &claude_five_hour)
            .replace("{claude_week}", &claude_week)
            .replace("{refreshing}", refreshing)
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
            label: i18n("agent-usage-session"),
            window,
        });
    }
    if let Some(window) = usage.weekly.as_ref() {
        rows.push(UsageWindowRow {
            label: i18n("agent-usage-weekly"),
            window,
        });
    }
    if let Some(window) = usage.fable_weekly.as_ref() {
        rows.push(UsageWindowRow {
            label: i18n("agent-usage-fable-weekly"),
            window,
        });
    }
    rows
}

fn format_window_duration(window_minutes: u32) -> String {
    if window_minutes.is_multiple_of(24 * 60) {
        i18n("agent-usage-duration-days")
            .replace("{count}", &(window_minutes / (24 * 60)).to_string())
    } else if window_minutes.is_multiple_of(60) {
        i18n("agent-usage-duration-hours").replace("{count}", &(window_minutes / 60).to_string())
    } else {
        i18n("agent-usage-duration-minutes").replace("{count}", &window_minutes.to_string())
    }
}

fn format_duration_until(timestamp: i64, now: i64) -> String {
    let remaining = timestamp.saturating_sub(now);
    if remaining <= 0 {
        return i18n("agent-usage-duration-now").to_string();
    }

    let total_minutes = remaining.saturating_add(59_999) / 60_000;
    if total_minutes < 60 {
        return i18n("agent-usage-duration-minutes").replace("{count}", &total_minutes.to_string());
    }

    let total_hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if total_hours < 24 {
        return if minutes == 0 {
            i18n("agent-usage-duration-hours").replace("{count}", &total_hours.to_string())
        } else {
            i18n("agent-usage-duration-hours-minutes")
                .replace("{hours}", &total_hours.to_string())
                .replace("{minutes}", &minutes.to_string())
        };
    }

    let days = total_hours / 24;
    let hours = total_hours % 24;
    if hours == 0 {
        i18n("agent-usage-duration-days").replace("{count}", &days.to_string())
    } else {
        i18n("agent-usage-duration-days-hours")
            .replace("{days}", &days.to_string())
            .replace("{hours}", &hours.to_string())
    }
}

fn format_reset_label(window: &UsageWindow, now: i64) -> Option<String> {
    window
        .resets_at
        .map(|timestamp| match format_duration_until(timestamp, now) {
            duration if duration == i18n("agent-usage-duration-now") => {
                i18n("agent-usage-resets-now").to_string()
            }
            duration => i18n("agent-usage-resets-in").replace("{duration}", &duration),
        })
        .or_else(|| window.reset_description.clone())
}

fn format_updated_label(usage: &UsageSnapshot, refreshing: bool, failed: bool, now: i64) -> String {
    if refreshing {
        return i18n("agent-usage-refreshing").to_string();
    }
    if failed && usage.updated_at.is_none() {
        return i18n("agent-usage-unavailable").to_string();
    }

    let Some(updated_at) = usage.updated_at else {
        return i18n("agent-usage-waiting").to_string();
    };
    let elapsed = now.saturating_sub(updated_at);
    let age = if elapsed < 60_000 {
        i18n("agent-usage-just-now").to_string()
    } else if elapsed < 60 * 60_000 {
        i18n("agent-usage-minutes-ago").replace("{count}", &(elapsed / 60_000).to_string())
    } else {
        i18n("agent-usage-hours-ago").replace("{count}", &(elapsed / (60 * 60_000)).to_string())
    };

    if failed {
        i18n("agent-usage-refresh-failed").replace("{age}", &age)
    } else {
        i18n("agent-usage-updated").replace("{age}", &age)
    }
}

fn reset_credit_label(usage: &UsageSnapshot, now: i64) -> Option<String> {
    let credits = usage.reset_credits.as_ref()?;
    let count_label = match credits.available_count {
        1 => i18n("agent-usage-one-reset-available").to_string(),
        count => i18n("agent-usage-many-resets-available").replace("{count}", &count.to_string()),
    };
    Some(match credits.next_expires_at {
        Some(expires_at) => match format_duration_until(expires_at, now) {
            duration if duration == i18n("agent-usage-duration-now") => {
                i18n("agent-usage-next-expires-now").replace("{count}", &count_label)
            }
            duration => i18n("agent-usage-next-expires-in")
                .replace("{count}", &count_label)
                .replace("{duration}", &duration),
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
                        .child(
                            i18n("agent-usage-window-label")
                                .replace("{name}", row.label)
                                .replace(
                                    "{duration}",
                                    &format_window_duration(row.window.window_minutes),
                                ),
                        ),
                )
                .child(div().flex_none().text_xs().text_color(colors.muted).child(
                    i18n("agent-usage-percent-left").replace("{percent}", &remaining.to_string()),
                )),
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
    let plan = usage
        .plan_type
        .as_ref()
        .map(|plan| i18n("agent-usage-plan").replace("{name}", plan));

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
                    .child(i18n("agent-usage-no-limits")),
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
        let refreshing = self.codex_refresh.refreshing || self.claude_refresh.refreshing;
        let codex_gauge = quota_gauge(
            "agent-usage-codex",
            i18n("agent-provider-codex"),
            Icon::new(CodexIcon),
            &self.codex,
            cx,
        );
        let claude_gauge = quota_gauge(
            "agent-usage-claude",
            i18n("agent-provider-claude"),
            Icon::new(ClaudeIcon),
            &self.claude,
            cx,
        );

        // Neither provider reports a limit at all, so there is nothing to
        // gauge. A row of bare icons over empty tracks would read as two
        // exhausted subscriptions rather than as two unknown ones.
        if codex_gauge.is_none() && claude_gauge.is_none() {
            return div().into_any_element();
        }

        // The divider separates two gauges; with one of them absent it would
        // be an edge against nothing.
        let divider = (codex_gauge.is_some() && claude_gauge.is_some()).then(|| {
            div()
                .flex_none()
                .w(px(1.))
                .h(px(12.))
                .bg(cx.theme().sidebar_foreground.opacity(0.15))
        });
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
            .h(px(QUOTA_ROW_HEIGHT))
            .px_1()
            .aria_label(self.accessibility_label())
            // Opacity communicates in-flight work without replacing or moving
            // the last successful values in this tightly packed status line.
            .when(refreshing, |this| this.opacity(0.65))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().sidebar_foreground.opacity(0.65))
                    .children(codex_gauge)
                    .children(divider)
                    .children(claude_gauge),
            )
            .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx)));

        div()
            .w_full()
            .child(
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
                                i18n("agent-provider-codex"),
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
                                i18n("agent-provider-claude"),
                                Icon::new(ClaudeIcon).small().into_any_element(),
                                &claude,
                                claude_refreshing,
                                claude_failed,
                                now,
                                colors,
                            ))
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
