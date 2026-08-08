use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, IconNamed, Sizable as _, h_flex};
use nmt_agent_utils::claude_code::usage_fetcher as claude_usage;
use nmt_agent_utils::codex::usage_fetcher as codex_usage;
use nmt_agent_utils::usage::UsageSnapshot;
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
                        Ok(usage) => this.codex = usage,
                        Err(err) => warn!("Codex usage refresh failed: {err}"),
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
                        Ok(usage) => this.claude = usage,
                        Err(err) => {
                            let retry = this.enabled && err == "Claude usage request cancelled";
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

impl Render for AgentUsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let [codex_five_hour, codex_week] = self.codex.compact_values();
        let [claude_five_hour, claude_week] = self.claude.compact_values();
        let refreshing = self.codex_refresh.refreshing || self.claude_refresh.refreshing;

        Button::new("agent-usage")
            .ghost()
            .small()
            .w_full()
            .h(px(28.))
            .px_1()
            .tooltip("Refresh Codex and Claude usage")
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
            .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_projection_keeps_provider_and_window_order() {
        let view = AgentUsageView {
            codex: UsageSnapshot {
                five_hour_remaining: Some(25),
                weekly_remaining: Some(80),
            },
            claude: UsageSnapshot {
                five_hour_remaining: Some(3),
                weekly_remaining: None,
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
}
