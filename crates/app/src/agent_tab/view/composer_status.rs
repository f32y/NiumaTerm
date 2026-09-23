#[cfg(test)]
#[path = "composer_status_tests.rs"]
mod tests;

use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, AsyncApp, Context, WeakEntity, div, px};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex};
use nmt_agent::git;
use nmt_agent::session::controller::SessionController;
use nmt_agent::transcript::turns::GenerationSpeed;
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::context_usage::{ContextUsageIndicator, cache_hit_percent};
use crate::agent_tab::settings::AgentSettings;
use crate::utils::on_runtime;

/// The footer under the composer card: the branch the pane's directory is on,
/// and what the conversation has spent so far. It owns the branch readout and
/// the background reads that keep it current.
#[derive(Default)]
pub(crate) struct ComposerStatusBar {
    branch: GitBranchPoll,
}

/// The status footer along the bottom edge of the composer card. It reports
/// rather than invites input, so it is set below the chrome size to keep the
/// prompt above it the loudest thing on the card.
const COMPOSER_STATUS_PADDING_X: f32 = 14.0;

const COMPOSER_STATUS_PADDING_Y: f32 = 6.0;
const COMPOSER_STATUS_TEXT_SIZE: f32 = 11.5;

/// Background-refreshed git branch of the pane's working directory.
#[derive(Default)]
pub(super) struct GitBranchPoll {
    branch: Option<String>,
    ready: bool,
    refreshing: bool,
    generation: u64,
}

impl GitBranchPoll {
    fn invalidate(&mut self) {
        self.generation += 1;
        self.branch = None;
        self.ready = false;
        self.refreshing = false;
    }

    fn begin_refresh(&mut self) -> Option<u64> {
        if self.refreshing {
            return None;
        }

        self.refreshing = true;

        Some(self.generation)
    }

    fn complete(&mut self, generation: u64, branch: Option<String>) {
        if generation != self.generation {
            return;
        }

        self.branch = branch;
        self.ready = true;
        self.refreshing = false;
    }

    fn presentation(&self) -> (String, f32) {
        let label = self.branch.clone().unwrap_or_else(|| {
            if self.ready {
                t!("agent-git-no-branch").to_string()
            } else {
                t!("agent-git-detecting-branch").to_string()
            }
        });

        let opacity = if self.branch.is_some() { 0.72 } else { 0.48 };

        (label, opacity)
    }
}

impl ComposerStatusBar {
    /// Forget the branch after the pane moved to another directory, so a read
    /// still under way for the old one cannot land.
    pub(crate) fn invalidate_branch(&mut self) {
        self.branch.invalidate();
    }

    /// Read the branch `cwd` is on in the background, unless a read is
    /// already under way. A pane with no directory has no branch to show.
    pub(crate) fn refresh_branch(&mut self, cwd: Option<String>, cx: &mut Context<AgentPane>) {
        let Some(generation) = self.branch.begin_refresh() else {
            return;
        };

        let Some(cwd) = cwd else {
            self.branch.complete(generation, None);

            return;
        };

        // Every tab open on this directory asks the same question on the same
        // interval, so an answer read within one is theirs to share.
        let max_age = Duration::from_secs(
            cx.global::<AgentSettings>()
                .git_status_refresh_interval
                .max(1),
        );

        cx.spawn(async move |this, cx| {
            let branch = on_runtime(async move { branch_label(&cwd, max_age).await }).await;

            this.update(cx, |this, cx| {
                this.composer_status.branch.complete(generation, branch);

                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The footer for `session`, whose newest turn took `steps` actions.
    pub(crate) fn render(
        &self,
        session: &SessionController,
        steps: usize,
        cx: &mut Context<AgentPane>,
    ) -> AnyElement {
        let (branch, branch_opacity) = self.branch.presentation();

        let shared = session.conversation().clone();
        let conversation = shared.borrow();

        let usage = conversation.context_window_usage.map(|usage| {
            ContextUsageIndicator::new(
                usage,
                conversation.context_composition.clone(),
                conversation.session_stats,
            )
        });

        // A backend that folds the count from its whole log is authoritative:
        // this side's counter sees only the turns it replayed, and a replay is
        // one page rather than the conversation.
        let turns = conversation
            .session_stats
            .map(|stats| stats.turns)
            .unwrap_or(session.turn());

        let speed = conversation.generation_stats.speed();

        let stats = composer_stats_label(
            turns,
            steps,
            conversation.first_output_latency,
            conversation
                .context_window_usage
                .and_then(cache_hit_percent),
            speed,
        );

        h_flex()
            .w_full()
            .min_h(px(24.))
            // No rule and no fill of its own: the readouts are quiet text
            // resting on the pane, and an edge under the composer would read
            // as a second card boundary right below the card's own.
            .px(px(COMPOSER_STATUS_PADDING_X))
            .py(px(COMPOSER_STATUS_PADDING_Y))
            .items_center()
            .justify_between()
            .gap_3()
            // Everything the footer reports is an identifier or a figure — a
            // branch name, turn counts, timings, percentages — so the whole
            // strip is set in the code face rather than each readout choosing
            // for itself and the context indicator between them falling back
            // to the prose face.
            .font(cx.global::<AgentSettings>().transcript_font())
            .text_size(px(COMPOSER_STATUS_TEXT_SIZE))
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .items_center()
                    .text_color(cx.theme().muted_foreground.opacity(branch_opacity))
                    .child(Icon::new(IconName::GitBranch).size_3())
                    .child(div().min_w_0().truncate().child(branch)),
            )
            .child(
                // The readouts belong with the context indicator rather than
                // centered between it and the branch: both report what the
                // conversation has spent, and a variable-width group in the
                // middle would drift as its parts appear.
                h_flex()
                    .flex_none()
                    .gap_3()
                    .items_center()
                    .children(stats.map(|stats| {
                        div()
                            .id("agent-composer-stats")
                            .aria_label(
                                t!("agent-status-accessibility", stats = &stats).into_owned(),
                            )
                            .text_color(cx.theme().muted_foreground.opacity(0.72))
                            .when_some(speed, |this, speed| {
                                this.tooltip(move |window, cx| {
                                    Tooltip::new(
                                        t!(if speed.estimated {
                                            "agent-status-generation-estimated-tooltip"
                                        } else {
                                            "agent-status-generation-tooltip"
                                        })
                                        .into_owned(),
                                    )
                                    .build(window, cx)
                                })
                            })
                            .child(stats)
                    }))
                    .children(usage),
            )
            .into_any_element()
    }
}

/// Re-read the pane's branch on the configured interval for as long as the
/// pane lives.
pub(crate) async fn poll_git_branch(this: WeakEntity<AgentPane>, cx: &mut AsyncApp) {
    loop {
        let Ok(interval) = this.update(cx, |_, cx| {
            cx.global::<AgentSettings>().git_status_refresh_interval
        }) else {
            break;
        };

        cx.background_executor()
            .timer(Duration::from_secs(interval.max(1)))
            .await;

        if this
            .update(cx, |this, cx| this.refresh_git_branch(cx))
            .is_err()
        {
            break;
        }
    }
}

/// The composer's one-line account of the conversation: how many turns it has
/// run, how many actions the newest turn took, how long that turn waited for
/// its first output, how much of the input the provider had cached, and the
/// model's generation speed. Missing readings are omitted. A generation sample
/// can precede the backend's first completed-turn counter.
pub(super) fn composer_stats_label(
    turns: u64,
    steps: usize,
    first_output: Option<Duration>,
    cache_hit: Option<u64>,
    speed: Option<GenerationSpeed>,
) -> Option<String> {
    if turns == 0 && speed.is_none() {
        return None;
    }

    let mut parts = Vec::new();

    if turns > 0 {
        parts.push(t!("agent-status-turns", count = turns).into_owned());
    }

    if steps > 0 {
        parts.push(t!("agent-status-steps", count = steps).into_owned());
    }

    if let Some(first_output) = first_output {
        parts.push(
            t!(
                "agent-status-first-output",
                value = &latency_readout(first_output)
            )
            .into_owned(),
        );
    }

    if let Some(percent) = cache_hit {
        parts.push(t!("agent-status-cache-hit", percent = percent).into_owned());
    }

    if let Some(speed) = speed {
        let prefix = if speed.estimated { "~" } else { "" };

        parts.push(
            t!(
                "agent-status-generation-speed",
                value = format!("{prefix}{:.1}", speed.tokens_per_second)
            )
            .into_owned(),
        );
    }

    Some(parts.join(" · "))
}

/// Sub-second latencies are the interesting ones, and a reading like `0.8s`
/// hides how much of a second it was; past a second the tenth is enough.
fn latency_readout(latency: Duration) -> String {
    if latency < Duration::from_secs(1) {
        format!("{}ms", latency.as_millis())
    } else {
        format!("{:.1}s", latency.as_secs_f64())
    }
}

/// The pane's branch label: a detached `HEAD` shows its short commit,
/// matching the git footer's presentation of the same state.
async fn branch_label(cwd: &str, max_age: Duration) -> Option<String> {
    let branch = match git::current_branch(cwd, max_age).await {
        Ok(branch) => branch?,
        Err(_) => return Some(t!("agent-composer-git-unavailable").into_owned()),
    };

    Some(match branch {
        git::CheckedOut::Branch(branch) => branch,
        git::CheckedOut::Detached(commit) => {
            t!("git-status-detached", commit = &commit).into_owned()
        }
    })
}
