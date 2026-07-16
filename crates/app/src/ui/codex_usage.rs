//! Workspace-sidebar widget showing the active Codex account's remaining rate limits.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::progress::Progress;
use gpui_component::{Sizable as _, h_flex, v_flex};
use nmt_agent_utils::codex::usage_fetcher::{self, Usage};

use crate::ui::AppSettings;

const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

pub(crate) struct CodexUsageView {
    usage: Usage,
    refreshing: bool,
    enabled: bool,
}

impl CodexUsageView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let enabled = cx.global::<AppSettings>().show_agent_usage;
        cx.observe_global::<AppSettings>(|this, cx| {
            let enabled = cx.global::<AppSettings>().show_agent_usage;
            if enabled && !this.enabled {
                this.refresh(cx);
            }
            this.enabled = enabled;
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH_INTERVAL).await;
                if this
                    .update(cx, |this, cx| {
                        if this.enabled {
                            this.refresh(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let mut this = Self {
            usage: Usage::default(),
            refreshing: false,
            enabled,
        };
        if enabled {
            this.refresh(cx);
        }
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        cx.notify();
        let fetch = cx
            .background_executor()
            .spawn(async move { usage_fetcher::fetch() });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                this.refreshing = false;
                match result {
                    Ok(usage) => this.usage = usage,
                    Err(err) => tracing::warn!("Codex usage refresh failed: {err}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for CodexUsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.usage == Usage::default() {
            return div().into_any_element();
        }

        let rows = [
            ("codex-usage-5h", "5h", self.usage.five_hour),
            ("codex-usage-week", "Week", self.usage.weekly),
        ]
        .into_iter()
        .filter_map(|(id, label, value)| {
            value.map(|value| {
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(div().w_8().flex_none().text_xs().child(label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Progress::new(id).xsmall().value(value as f32)),
                    )
            })
        });

        Button::new("codex-usage")
            .ghost()
            .small()
            .w_full()
            .h_auto()
            .px_0()
            .py_1()
            .child(v_flex().w_full().gap_2().children(rows))
            .loading(self.refreshing)
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
            .into_any_element()
    }
}
