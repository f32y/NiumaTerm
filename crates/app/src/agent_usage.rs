#[cfg(test)]
#[path = "agent_usage_tests.rs"]
mod agent_usage_tests;

use std::borrow::Cow;
use std::time::Duration;

use app::agent_tab::profile::{ClaudeIcon, CodexIcon};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, FontWeight, Hsla, Window, div, px, relative};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::hover_card::HoverCard;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use nmt_agent::launcher::AgentCli;
use nmt_agent::usage::{UsageSnapshot, UsageWindow, now_unix_millis};
use rust_i18n::t;
use tracing::warn;

use crate::ui::AppSettings;
use crate::usage_refresh::{Completion, Refresh};
use crate::usage_sources::{account_sources, codex_source, codex_usage_launcher};

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

pub(crate) struct AgentUsageView {
    providers: [Refresh<UsageSnapshot>; 2],
    enabled: bool,
    codex_launcher: AgentCli,
}

impl AgentUsageView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let enabled = cx.global::<AppSettings>().config().agent.show_agent_usage;
        let codex_launcher = codex_usage_launcher(cx.global::<AppSettings>());

        let mut this = Self {
            providers: account_sources(codex_launcher.clone())
                .map(|source| Refresh::new(UsageSnapshot::default(), source, enabled)),
            enabled,
            codex_launcher,
        };

        cx.observe_global::<AppSettings>(Self::on_settings_changed)
            .detach();

        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(REFRESH_INTERVAL).await;

                if view.update(cx, |this, cx| this.refresh_all(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        this.refresh_all(cx);

        this
    }

    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let settings = cx.global::<AppSettings>();
        let enabled = settings.config().agent.show_agent_usage;
        let launcher = codex_usage_launcher(settings);
        let launcher_changed = launcher != self.codex_launcher;

        if launcher_changed {
            self.codex_launcher = launcher.clone();

            self.providers[0] =
                Refresh::new(UsageSnapshot::default(), codex_source(launcher), enabled);
        }

        if enabled != self.enabled {
            self.enabled = enabled;

            for provider in &mut self.providers {
                provider.set_enabled(enabled);
            }

            if enabled {
                self.refresh_all(cx);
            }
        } else if launcher_changed {
            self.refresh_provider(0, cx);
        }
    }

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        for index in 0..self.providers.len() {
            self.refresh_provider(index, cx);
        }
    }

    fn refresh_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(fetch) = self.providers[index].begin() else {
            return;
        };

        cx.notify();

        let worker = cx.background_executor().spawn(async move { fetch.run() });

        cx.spawn(async move |view, cx| {
            let fetched = worker.await;

            let _ = view.update(cx, |this, cx| {
                match this.providers[index].complete(fetched) {
                    Completion::Retry => this.refresh_provider(index, cx),

                    Completion::Failed(message) => {
                        warn!(provider = index, "account usage refresh failed: {message}")
                    }

                    Completion::Updated | Completion::Discarded => {}
                }

                cx.notify();
            });
        })
        .detach();
    }

    fn accessibility_label(&self) -> String {
        let [codex_five_hour, codex_week] = self.providers[0].value.compact_values();
        let [claude_five_hour, claude_week] = self.providers[1].value.compact_values();

        let refreshing = if self.providers[0].refreshing() || self.providers[1].refreshing() {
            t!("agent-usage-accessibility-refreshing")
        } else {
            "".into()
        };

        t!(
            "agent-usage-accessibility",
            codex_session = &codex_five_hour,
            codex_week = &codex_week,
            claude_session = &claude_five_hour,
            claude_week = &claude_week,
            refreshing = refreshing
        )
        .into_owned()
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
    label: Cow<'static, str>,
    window: &'a UsageWindow,
}

impl Render for AgentUsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let refreshing = self.providers[0].refreshing() || self.providers[1].refreshing();

        let codex_gauge = quota_gauge(
            "agent-usage-codex",
            t!("agent-provider-codex"),
            Icon::new(CodexIcon),
            &self.providers[0].value,
            cx,
        );

        let claude_gauge = quota_gauge(
            "agent-usage-claude",
            t!("agent-provider-claude"),
            Icon::new(ClaudeIcon),
            &self.providers[1].value,
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

        let codex = self.providers[0].value.clone();
        let claude = self.providers[1].value.clone();
        let codex_refreshing = self.providers[0].refreshing();
        let claude_refreshing = self.providers[1].refreshing();
        let codex_failed = self.providers[0].failed;
        let claude_failed = self.providers[1].failed;

        let trigger = Button::new("agent-usage")
            .ghost()
            .small()
            .w_full()
            .h(px(QUOTA_ROW_HEIGHT))
            .px_1()
            .accessibility_label(self.accessibility_label())
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
                        render_usage_details(
                            &codex,
                            &claude,
                            codex_refreshing,
                            claude_refreshing,
                            codex_failed,
                            claude_failed,
                            cx,
                        )
                    }),
            )
            .into_any_element()
    }
}

fn usage_window_rows(usage: &UsageSnapshot) -> Vec<UsageWindowRow<'_>> {
    let mut rows = Vec::with_capacity(3);

    if let Some(window) = usage.five_hour.as_ref() {
        rows.push(UsageWindowRow {
            label: t!("agent-usage-session"),
            window,
        });
    }

    if let Some(window) = usage.weekly.as_ref() {
        rows.push(UsageWindowRow {
            label: t!("agent-usage-weekly"),
            window,
        });
    }

    if let Some(window) = usage.fable_weekly.as_ref() {
        rows.push(UsageWindowRow {
            label: t!("agent-usage-fable-weekly"),
            window,
        });
    }

    rows
}

fn format_window_duration(window_minutes: u32) -> String {
    if window_minutes.is_multiple_of(24 * 60) {
        t!(
            "agent-usage-duration-days",
            count = (window_minutes / (24 * 60))
        )
        .into_owned()
    } else if window_minutes.is_multiple_of(60) {
        t!("agent-usage-duration-hours", count = (window_minutes / 60)).into_owned()
    } else {
        t!("agent-usage-duration-minutes", count = window_minutes).into_owned()
    }
}

fn format_duration_until(timestamp: i64, now: i64) -> String {
    let remaining = timestamp.saturating_sub(now);

    if remaining <= 0 {
        return t!("agent-usage-duration-now").to_string();
    }

    let total_minutes = remaining.saturating_add(59_999) / 60_000;

    if total_minutes < 60 {
        return t!("agent-usage-duration-minutes", count = total_minutes).into_owned();
    }

    let total_hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if total_hours < 24 {
        return if minutes == 0 {
            t!("agent-usage-duration-hours", count = total_hours).into_owned()
        } else {
            t!(
                "agent-usage-duration-hours-minutes",
                hours = total_hours,
                minutes = minutes
            )
            .into_owned()
        };
    }

    let days = total_hours / 24;
    let hours = total_hours % 24;

    if hours == 0 {
        t!("agent-usage-duration-days", count = days).into_owned()
    } else {
        t!(
            "agent-usage-duration-days-hours",
            days = days,
            hours = hours
        )
        .into_owned()
    }
}

fn format_reset_label(window: &UsageWindow, now: i64) -> Option<String> {
    window
        .resets_at
        .map(|timestamp| match format_duration_until(timestamp, now) {
            duration if duration == t!("agent-usage-duration-now") => {
                t!("agent-usage-resets-now").to_string()
            }

            duration => t!("agent-usage-resets-in", duration = &duration).into_owned(),
        })
        .or_else(|| window.reset_description.clone())
}

fn format_updated_label(usage: &UsageSnapshot, refreshing: bool, failed: bool, now: i64) -> String {
    if refreshing {
        return t!("agent-usage-refreshing").to_string();
    }

    if failed && usage.updated_at.is_none() {
        return t!("agent-usage-unavailable").to_string();
    }

    let Some(updated_at) = usage.updated_at else {
        return t!("agent-usage-waiting").to_string();
    };

    let elapsed = now.saturating_sub(updated_at);

    let age = if elapsed < 60_000 {
        t!("agent-usage-just-now").to_string()
    } else if elapsed < 60 * 60_000 {
        t!("agent-usage-minutes-ago", count = (elapsed / 60_000)).into_owned()
    } else {
        t!("agent-usage-hours-ago", count = (elapsed / (60 * 60_000))).into_owned()
    };

    if failed {
        t!("agent-usage-refresh-failed", age = &age).into_owned()
    } else {
        t!("agent-usage-updated", age = &age).into_owned()
    }
}

fn reset_credit_label(usage: &UsageSnapshot, now: i64) -> Option<String> {
    let credits = usage.reset_credits.as_ref()?;

    let count_label = match credits.available_count {
        1 => t!("agent-usage-one-reset-available").to_string(),

        count => t!("agent-usage-many-resets-available", count = count).into_owned(),
    };

    Some(match credits.next_expires_at {
        Some(expires_at) => match format_duration_until(expires_at, now) {
            duration if duration == t!("agent-usage-duration-now") => {
                t!("agent-usage-next-expires-now", count = &count_label).into_owned()
            }

            duration => t!(
                "agent-usage-next-expires-in",
                count = &count_label,
                duration = &duration
            )
            .into_owned(),
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
    let percentage: f32 = remaining.into();

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
                            t!(
                                "agent-usage-window-label",
                                name = row.label,
                                duration = &format_window_duration(row.window.window_minutes)
                            )
                            .into_owned(),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(colors.muted)
                        .child(t!("agent-usage-percent-left", percent = remaining).into_owned()),
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
                        .w(relative(percentage / 100.0))
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
    name: Cow<'static, str>,
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
        .map(|plan| t!("agent-usage-plan", name = plan).into_owned());

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
                    .child(t!("agent-usage-no-limits")),
            )
        })
        .children(
            rows.into_iter()
                .map(|row| render_usage_window(row, now, colors)),
        )
        .into_any_element()
}

fn render_usage_details(
    codex: &UsageSnapshot,
    claude: &UsageSnapshot,
    codex_refreshing: bool,
    claude_refreshing: bool,
    codex_failed: bool,
    claude_failed: bool,
    cx: &App,
) -> gpui::Div {
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
            t!("agent-provider-codex"),
            Icon::new(CodexIcon).small().into_any_element(),
            codex,
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
            t!("agent-provider-claude"),
            Icon::new(ClaudeIcon).small().into_any_element(),
            claude,
            claude_refreshing,
            claude_failed,
            now,
            colors,
        ))
}

/// One provider half of the quota row: its mark, how much of the window it
/// reports is left, and that same figure spelled out. A provider that reports
/// no window at all gets no gauge, because an empty track would state a limit
/// nothing was measured against.
fn quota_gauge(
    id: &'static str,
    label: Cow<'static, str>,
    icon: Icon,
    usage: &UsageSnapshot,
    cx: &App,
) -> Option<AnyElement> {
    let window = usage.compact_window()?;
    let value = format!("{}%", window.remaining_percentage);
    let remaining: f32 = window.remaining_percentage.into();

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
                            .w(relative(remaining / 100.0))
                            .rounded_full()
                            .bg(cx.theme().primary.opacity(QUOTA_FILL_OPACITY)),
                    ),
            )
            .child(div().flex_none().child(value))
            .into_any_element(),
    )
}
