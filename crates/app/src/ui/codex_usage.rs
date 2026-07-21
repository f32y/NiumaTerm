//! Workspace-sidebar widget showing the active Codex account's remaining rate limits.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::progress::Progress;
use gpui_component::{Sizable as _, h_flex, v_flex};
use nmt_agent_utils::codex::usage_fetcher::{self, Usage};

use crate::ui::AppSettings;
use crate::ui::auto_refresh::{self, AutoRefresh, RefreshState};

pub(crate) struct CodexUsageView {
    usage: Usage,
    state: RefreshState,
}

impl AutoRefresh for CodexUsageView {
    type Output = Result<Usage, String>;
    const INTERVAL: Duration = Duration::from_secs(15 * 60);

    fn enabled(settings: &AppSettings) -> bool {
        settings.show_agent_usage
    }

    fn state(&mut self) -> &mut RefreshState {
        &mut self.state
    }

    fn fetch() -> Self::Output {
        usage_fetcher::fetch()
    }

    fn apply(&mut self, output: Self::Output) {
        match output {
            Ok(usage) => self.usage = usage,
            Err(err) => tracing::warn!("Codex usage refresh failed: {err}"),
        }
    }
}

impl CodexUsageView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            usage: Usage::default(),
            state: RefreshState {
                refreshing: false,
                enabled: Self::enabled(cx.global::<AppSettings>()),
            },
        };
        auto_refresh::start(&mut this, cx);
        this
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
            .loading(self.state.refreshing)
            .on_click(cx.listener(|this, _, _, cx| auto_refresh::refresh(this, cx)))
            .into_any_element()
    }
}
