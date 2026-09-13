use gpui::Context;
use nmt_agent::chat::{Event, QuestionMode, SlashCommandOutcome, TurnActivity};
use nmt_agent::session::controller::SessionEffect;
use nmt_agent::session::lifecycle::RecoverySnapshot;
use nmt_agent::{AgentEvent, AgentEventKind, normalize_body, normalize_title};
use rust_i18n::t;

use crate::agent_tab::AgentPaneEvent;
use crate::agent_tab::execution::AgentSession;

impl AgentSession {
    pub(crate) fn on_event(&mut self, epoch: u64, event: Event, cx: &mut Context<Self>) {
        if self.is_closed() || !self.controller.borrow().runtime.is_current(epoch) {
            return;
        }

        self.prepare_defaults(cx);

        let event = match event {
            Event::HostExited { message } => {
                let mut state = self.controller.borrow_mut();

                let identity = state
                    .runtime
                    .backend()
                    .and_then(|backend| backend.recovery_identity());

                state.runtime.reconnect(Some(RecoverySnapshot {
                    identity,
                    profile_name: self.profile.name.clone(),
                }));

                state.runtime.recovery_failed(message.clone());

                Event::Error {
                    message,
                    fatal: true,
                }
            }

            event => event,
        };

        let effect = self.controller.borrow_mut().apply_event(epoch, event);

        if let SessionEffect::Branch(update) = effect {
            self.on_branch_update(update, cx);

            return;
        }

        if let SessionEffect::StatusDetail(detail) = &effect {
            let detail = detail.as_ref().map(|activity| match activity {
                TurnActivity::Retrying {
                    attempt,
                    total,
                    reason,
                } => t!(
                    "agent-transcript-retrying",
                    attempt = attempt,
                    total = total,
                    reason = reason
                )
                .into_owned(),
            });

            let state = self.controller.borrow();
            let mut conversation = state.conversation.borrow_mut();

            conversation.live.set_detail(detail);
            conversation.changed_turn(state.delivery.turn());
        }

        self.publish_activity(&effect, cx);

        match &effect {
            SessionEffect::Ready(_) => {
                self.restore_background_tasks(cx);
                self.restore_workflows(cx);
            }

            SessionEffect::InputRequested { index } => self.expire_optional_question(*index, cx),
            SessionEffect::Workflows { .. } => self.sync_workflow_refresh(cx),
            _ => {}
        }

        self.publish(effect, cx);
        self.advance_commands(cx);
    }

    pub(crate) fn advance_commands(&mut self, cx: &mut Context<Self>) {
        if self.is_closed() {
            return;
        }

        loop {
            let next = self.controller.borrow_mut().next_command();

            let Some((name, outcome)) = next else {
                break;
            };

            let accepted = matches!(outcome, SlashCommandOutcome::Accepted);

            self.publish(
                SessionEffect::CommandResult {
                    name,
                    outcome,
                    advance: false,
                },
                cx,
            );

            if accepted {
                break;
            }
        }
    }

    fn publish_activity(&mut self, effect: &SessionEffect, cx: &mut Context<Self>) {
        match effect {
            SessionEffect::Title(title) => cx.emit(AgentPaneEvent::TitleSuggested(title.clone())),

            SessionEffect::TurnStarted { .. } => {
                self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx)
            }

            SessionEffect::TurnCompleted { error, .. } => {
                let state = self.controller.borrow();
                let key = (state.runtime.epoch(), state.delivery.turn());

                if self.last_completed == Some(key) {
                    return;
                }

                let body = error
                    .clone()
                    .or_else(|| {
                        state
                            .conversation
                            .borrow()
                            .content
                            .latest_agent_message(key.1)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| {
                        t!("agent-session-turn-completed", name = self.kind.display()).into_owned()
                    });

                drop(state);
                self.last_completed = Some(key);

                self.emit_lifecycle(
                    AgentEventKind::Stopped,
                    &t!(
                        "agent-session-provider-finished",
                        name = self.kind.display()
                    ),
                    &body,
                    cx,
                );
            }

            SessionEffect::ApprovalRequested => {
                let body = self
                    .controller
                    .borrow()
                    .input
                    .approval()
                    .unwrap_or_default()
                    .to_string();

                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &t!("agent-session-needs-input", name = self.kind.display()),
                    &body,
                    cx,
                );
            }

            SessionEffect::InputRequested { index } => {
                let state = self.controller.borrow();
                let prompt = &state.input.batches()[*index];

                if prompt.mode() == QuestionMode::Async {
                    return;
                }

                let body = prompt
                    .questions()
                    .first()
                    .map_or("", |question| question.question.as_str())
                    .to_string();

                drop(state);

                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &t!("agent-session-needs-input", name = self.kind.display()),
                    &body,
                    cx,
                );
            }

            SessionEffect::ApprovalResolved => {
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx)
            }

            SessionEffect::InputResolved(completion) => {
                if completion.started_turn {
                    self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
                }

                if completion.waiting_finished {
                    self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                }
            }

            SessionEffect::Workflows {
                activity_changed: true,
            } => cx.emit(AgentPaneEvent::WorkflowActivity),

            SessionEffect::BackgroundActivity => cx.emit(AgentPaneEvent::BackgroundTaskActivity),
            SessionEffect::Error { fatal: true, .. } => cx.emit(AgentPaneEvent::Interrupted),
            _ => {}
        }
    }

    pub(crate) fn emit_lifecycle(
        &self,
        kind: AgentEventKind,
        title: &str,
        body: &str,
        cx: &mut Context<Self>,
    ) {
        let state = self.controller.borrow();
        let agent: &str = self.kind.into();

        cx.emit(AgentPaneEvent::Lifecycle(AgentEvent {
            route: self.route.clone(),
            agent: agent.into(),
            session_id: self.id.0.to_string(),
            turn_id: (kind != AgentEventKind::SessionStarted)
                .then(|| format!("turn-{}", state.delivery.turn())),
            kind,
            title: normalize_title(title),
            body: normalize_body(body),
        }));
    }
}
