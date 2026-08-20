//! Read-only `Workflows` view: the Dynamic Workflow runs a Claude Code session
//! started, their phases, and the agents each one fanned out to.
//!
//! Workflow agents are not child agents — the provider reports them as entries
//! inside one run rather than as tasks of their own — so they are shown here
//! instead of in `Background Tasks`, which stays scoped to child agents. The
//! pane owns the run model and the refresh; this component only reads it and
//! never drives a run.

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, Hsla, ScrollHandle, SharedString, WeakEntity, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use nmt_agent_utils::workflow::{WorkflowAgent, WorkflowAgentState, WorkflowRun, WorkflowRunState};
use nmt_i18n::i18n;

use crate::agent::AgentPane;
use crate::agent::transcript::TranscriptView;
use crate::ui::AppSettings;

pub(crate) struct WorkflowsView {
    /// The Agent pane whose runs are shown. Weak because the tab can close
    /// while the panel is still mounted for its closing animation.
    target: Option<WeakEntity<AgentPane>>,
    /// Renders an open agent conversation with the parent conversation's own
    /// presentation, so a reader does not learn a second layout.
    detail_transcript: Option<Entity<TranscriptView>>,
    scroll: ScrollHandle,
    visible: bool,
}

impl WorkflowsView {
    pub(crate) fn new() -> Self {
        Self {
            target: None,
            detail_transcript: None,
            scroll: ScrollHandle::new(),
            visible: false,
        }
    }

    /// Point the view at another session. An open conversation belongs to one
    /// session, so it closes rather than carrying over.
    pub(crate) fn set_target(
        &mut self,
        target: Option<WeakEntity<AgentPane>>,
        cx: &mut Context<Self>,
    ) {
        let same = match (&self.target, &target) {
            (Some(current), Some(next)) => current == next,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        // The pane being replaced must stop refreshing for a view that is no
        // longer pointed at it.
        self.report_visibility(false, cx);
        self.target = target;
        self.detail_transcript = None;
        self.scroll = ScrollHandle::new();
        self.report_visibility(self.visible, cx);
        cx.notify();
    }

    /// Refreshing follows visibility, so the pane is told either way.
    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.report_visibility(visible, cx);
        cx.notify();
    }

    fn report_visibility(&self, visible: bool, cx: &mut Context<Self>) {
        let Some(pane) = self.target.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        pane.update(cx, |pane, cx| pane.set_workflows_visible(visible, cx));
    }

    fn runs(&self, cx: &Context<Self>) -> Vec<WorkflowRun> {
        self.target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|pane| pane.read(cx).workflow_runs().to_vec())
            .unwrap_or_default()
    }

    fn open_agent(&mut self, task_id: &str, agent_id: &str, cx: &mut Context<Self>) {
        let Some(pane) = self.target.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        let (kind, cwd) = {
            let pane = pane.read(cx);
            (pane.agent_kind(), pane.working_directory())
        };
        self.detail_transcript = Some(cx.new(|_| TranscriptView::new(kind, cwd)));
        pane.update(cx, |pane, cx| {
            pane.open_workflow_agent(task_id, agent_id, cx);
        });
        cx.notify();
    }

    fn close_agent(&mut self, cx: &mut Context<Self>) {
        self.detail_transcript = None;
        if let Some(pane) = self.target.as_ref().and_then(WeakEntity::upgrade) {
            pane.update(cx, |pane, cx| pane.close_workflow_agent(cx));
        }
        cx.notify();
    }
}

impl Render for WorkflowsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The sibling `Background Tasks` panel reads in the agent's own font,
        // and the two are the same surface to a user, so both follow the agent
        // typography rather than the app chrome's.
        let settings = cx.global::<AppSettings>();
        let font_family = settings.agent_font_family.clone();
        let font_size = px(settings.agent_font_size as f32);
        let showing_conversation = self.detail_transcript.is_some();

        v_flex()
            .size_full()
            // A flex item defaults to its content's size, so without these an
            // agent conversation wider than the panel pushes past its edge and
            // a long run list stretches the whole window row.
            .min_w_0()
            .overflow_hidden()
            .font_family(font_family)
            .text_size(font_size)
            // An open conversation carries its own header with the way back,
            // so the panel title stands down rather than stacking two bars.
            .children((!showing_conversation).then(|| {
                h_flex()
                    .px_2()
                    .py_1()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(div().text_sm().child(i18n("workflows-title")))
            }))
            .child(self.render_body(cx))
    }
}

impl WorkflowsView {
    fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pane) = self.target.as_ref().and_then(WeakEntity::upgrade) else {
            return empty_state(
                i18n("workflows-no-session"),
                i18n("workflows-no-session-detail"),
                cx,
            );
        };
        // Only Claude Code reports workflows; every other pane has none.
        if pane.read(cx).workflow_session_id().is_none() {
            return empty_state(
                i18n("workflows-no-session"),
                i18n("workflows-no-session-detail"),
                cx,
            );
        }

        if let Some(detail) = self.render_detail(&pane, cx) {
            return detail;
        }

        let runs = self.runs(cx);
        if runs.is_empty() {
            return empty_state(i18n("workflows-empty"), i18n("workflows-empty-detail"), cx);
        }

        v_flex()
            .id("workflows-list")
            // A scrollable child of a column needs a bounded height to scroll
            // within: sized to its content it would stretch the panel instead.
            .flex_1()
            .min_h_0()
            .w_full()
            .min_w_0()
            .py_1()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(
                runs.iter()
                    .map(|run| self.render_run(run, cx))
                    .collect::<Vec<_>>(),
            )
            .into_any_element()
    }

    fn render_run(&self, run: &WorkflowRun, cx: &mut Context<Self>) -> AnyElement {
        let task_id = run.task_id.clone();
        let state_color = run_state_color(run.state, cx);
        let muted = cx.theme().muted_foreground;

        let sections: Vec<AnyElement> = group_agents_by_phase(run)
            .into_iter()
            .map(|(title, agents)| {
                let agents: Vec<AnyElement> = agents
                    .into_iter()
                    .map(|agent| self.render_agent(&task_id, agent, cx))
                    .collect();

                v_flex()
                    .min_w_0()
                    .children(title.map(|title| {
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(muted)
                            .child(title.to_owned())
                    }))
                    .children(agents)
                    .into_any_element()
            })
            .collect();

        v_flex()
            .w_full()
            .min_w_0()
            .pb_1()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .child(run.display_label()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(state_color)
                            .child(run_state_label(run)),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted)
                    .child(run_totals(run)),
            )
            // A read that failed says so on the run it could not refresh; the
            // run's own state is unaffected by it.
            .children(run.refresh_failed.then(|| {
                div()
                    .px_2()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted)
                    .child(i18n("workflows-refresh-failed").to_string())
            }))
            .children(sections)
            .children(run.result.as_ref().map(|result| {
                div()
                    .px_2()
                    .py_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted)
                    .child(result.clone())
            }))
            .into_any_element()
    }

    fn render_agent(
        &self,
        task_id: &str,
        agent: &WorkflowAgent,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().red;
        let hover = cx.theme().list_hover;
        let state_color = agent_state_color(agent.state, cx);
        let label = agent
            .label
            .clone()
            .unwrap_or_else(|| format!("#{}", agent.index));
        let agent_id = agent.agent_id.clone();
        let task_id = task_id.to_owned();

        // Only reported details are shown; an absent one leaves no placeholder.
        let mut detail: Vec<String> = Vec::new();
        if let Some(agent_type) = agent.agent_type.as_ref() {
            detail.push(agent_type.clone());
        }
        if let Some(model) = agent.model.as_ref() {
            detail.push(model.clone());
        }
        if let Some(tokens) = agent.tokens {
            detail.push(i18n("workflows-agent-tokens").replace("{count}", &tokens.to_string()));
        }
        if let Some(tool_calls) = agent.tool_calls {
            detail.push(
                i18n("workflows-agent-tool-calls").replace("{count}", &tool_calls.to_string()),
            );
        }
        if agent.reused {
            detail.push(i18n("workflows-agent-reused").to_string());
        }

        let row = v_flex()
            .w_full()
            .min_w_0()
            .px_2()
            .py_1()
            .gap_0p5()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().truncate().text_sm().child(label))
                    .child(
                        div()
                            .text_xs()
                            .text_color(state_color)
                            .child(agent_state_label(agent.state)),
                    ),
            )
            .children((!detail.is_empty()).then(|| {
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted)
                    .child(detail.join(" \u{b7} "))
            }))
            .children(agent.error.as_ref().map(|error| {
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(danger)
                    .child(error.clone())
            }));

        // A row opens a conversation only once the provider has given the
        // agent an id, because that id names its transcript.
        match agent_id {
            Some(agent_id) => div()
                .id(SharedString::from(format!("workflow-agent-{agent_id}")))
                .w_full()
                .min_w_0()
                .cursor_pointer()
                .hover(move |this| this.bg(hover))
                .child(row)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_agent(&task_id, &agent_id, cx);
                }))
                .into_any_element(),
            None => row.into_any_element(),
        }
    }

    fn render_detail(
        &mut self,
        pane: &Entity<AgentPane>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let transcript = self.detail_transcript.clone()?;
        let (label, items, revision, unavailable) = {
            let pane = pane.read(cx);
            let open = pane.open_workflow_conversation()?;
            // The row's own label is what the user picked from; the agent id
            // is the fallback for a row that never got one.
            let label = pane
                .workflow_runs()
                .iter()
                .find(|run| run.task_id == open.task_id)
                .and_then(|run| run.agent(&open.agent_id))
                .and_then(|agent| agent.label.clone())
                .unwrap_or_else(|| open.agent_id.clone());
            (label, open.items.clone(), open.revision(), open.unavailable)
        };

        transcript.update(cx, |view, cx| view.show_items(&items, revision, cx));

        let body: AnyElement = if items.is_empty() && unavailable {
            empty_state(
                i18n("workflows-conversation-unavailable"),
                i18n("workflows-conversation-unavailable-detail"),
                cx,
            )
        } else {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .child(transcript.clone())
                .into_any_element()
        };

        Some(
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().sidebar_border)
                        .child(
                            Button::new("workflow-agent-back")
                                .ghost()
                                .xsmall()
                                .icon(IconName::ArrowLeft)
                                .on_click(cx.listener(|this, _, _, cx| this.close_agent(cx))),
                        )
                        .child(div().flex_1().truncate().text_sm().child(label)),
                )
                .child(body)
                .into_any_element(),
        )
    }
}

/// Two-line centered state, matching how the sibling panel reports having
/// nothing to show.
fn empty_state(title: &str, detail: &str, cx: &Context<WorkflowsView>) -> AnyElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_1()
        .px(px(16.0))
        .child(div().text_sm().child(title.to_string()))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail.to_string()),
        )
        .into_any_element()
}

fn run_state_color(state: WorkflowRunState, cx: &Context<WorkflowsView>) -> Hsla {
    let theme = cx.theme();
    match state {
        WorkflowRunState::Failed => theme.red,
        WorkflowRunState::Done => theme.green,
        WorkflowRunState::Stopped => theme.muted_foreground,
        WorkflowRunState::Starting | WorkflowRunState::Running => theme.foreground,
    }
}

fn agent_state_color(state: WorkflowAgentState, cx: &Context<WorkflowsView>) -> Hsla {
    let theme = cx.theme();
    match state {
        WorkflowAgentState::Failed => theme.red,
        WorkflowAgentState::Done => theme.green,
        // An agent the run outlived reads as inconclusive, not as a failure.
        WorkflowAgentState::Queued | WorkflowAgentState::Stopped => theme.muted_foreground,
        WorkflowAgentState::Running => theme.foreground,
    }
}

/// Agents grouped by the phases the provider declared, in provider order. A
/// run that declared no phases, or an agent naming one the run never listed,
/// still appears — under a trailing untitled group rather than being dropped.
fn group_agents_by_phase(run: &WorkflowRun) -> Vec<(Option<&str>, Vec<&WorkflowAgent>)> {
    let mut sections: Vec<(Option<&str>, Vec<&WorkflowAgent>)> = Vec::new();

    for phase in &run.phases {
        let agents: Vec<&WorkflowAgent> = run
            .agents
            .iter()
            .filter(|agent| agent.phase_index == Some(phase.index))
            .collect();
        if !agents.is_empty() {
            sections.push((Some(phase.title.as_str()), agents));
        }
    }

    let ungrouped: Vec<&WorkflowAgent> = run
        .agents
        .iter()
        .filter(|agent| {
            agent
                .phase_index
                .is_none_or(|index| !run.phases.iter().any(|phase| phase.index == index))
        })
        .collect();
    if !ungrouped.is_empty() {
        sections.push((None, ungrouped));
    }

    sections
}

fn run_state_label(run: &WorkflowRun) -> String {
    match run.state {
        WorkflowRunState::Starting => i18n("workflows-state-starting"),
        WorkflowRunState::Running => i18n("workflows-state-running"),
        WorkflowRunState::Done => i18n("workflows-state-done"),
        WorkflowRunState::Failed => i18n("workflows-state-failed"),
        WorkflowRunState::Stopped => i18n("workflows-state-stopped"),
    }
    .to_string()
}

fn agent_state_label(state: WorkflowAgentState) -> String {
    match state {
        WorkflowAgentState::Queued => i18n("workflows-agent-queued"),
        WorkflowAgentState::Running => i18n("workflows-agent-running"),
        WorkflowAgentState::Done => i18n("workflows-agent-done"),
        WorkflowAgentState::Failed => i18n("workflows-agent-failed"),
        WorkflowAgentState::Stopped => i18n("workflows-agent-stopped"),
    }
    .to_string()
}

/// Run-level totals, omitting any the provider has not reported.
fn run_totals(run: &WorkflowRun) -> String {
    let mut parts =
        vec![i18n("workflows-run-agents").replace("{count}", &run.agent_count().to_string())];
    if let Some(tokens) = run.total_tokens {
        parts.push(i18n("workflows-run-tokens").replace("{count}", &tokens.to_string()));
    }
    if let Some(tool_calls) = run.total_tool_calls {
        parts.push(i18n("workflows-run-tool-calls").replace("{count}", &tool_calls.to_string()));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests;
