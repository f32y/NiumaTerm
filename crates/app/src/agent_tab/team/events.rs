use gpui::Context;
use nmt_agent::team::attempt::BudgetScope;
use nmt_agent::team::discussion::PauseReason;
use nmt_agent::team::identity::{MemberId, OperationId, StageId};
use nmt_agent::team::moderation::ModeratorAction;
use nmt_agent::team::session::{AttemptEventKey, TeamError};
use serde::Deserialize;

use crate::agent_tab::execution::ExecutionSignal;
use crate::agent_tab::team::TeamRuntime;
use crate::agent_tab::team::dispatch::work_status;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionArguments {
    operation: OperationId,
    stage: StageId,
    action: DecisionAction,
    recipients: Vec<MemberId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DecisionAction {
    Invite,
    Report,
}

impl TeamRuntime {
    pub(super) fn on_execution(
        &mut self,
        member: MemberId,
        signal: &ExecutionSignal,
        cx: &mut Context<Self>,
    ) {
        if self.closed {
            return;
        }

        if let Err(error) = self.apply_execution(member, signal, cx) {
            self.error = Some(error.to_string());
        }

        self.schedule(cx);
    }

    fn apply_execution(
        &mut self,
        member: MemberId,
        signal: &ExecutionSignal,
        cx: &mut Context<Self>,
    ) -> Result<(), TeamError> {
        let Some(host) = self.hosts.get(&member) else {
            return Ok(());
        };

        let Some(id) = host.active else { return Ok(()) };

        let Some(attempt) = self
            .room()
            .attempts()
            .iter()
            .find(|attempt| attempt.id == id)
            .cloned()
        else {
            return Ok(());
        };

        let epoch = match signal {
            ExecutionSignal::Accepted { epoch, .. }
            | ExecutionSignal::Finished { epoch, .. }
            | ExecutionSignal::Decision { epoch, .. } => *epoch,
        };

        let key = AttemptEventKey {
            attempt: id,
            member,
            ownership: attempt.intent.ownership,
            backend_generation: epoch,
        };

        if epoch != attempt.intent.backend_generation {
            return Ok(());
        }

        match signal {
            ExecutionSignal::Accepted { id, .. } => {
                self.session.accept_attempt(key, id)?;
            }

            ExecutionSignal::Finished {
                id, error, text, ..
            } => {
                if attempt.provider_turn.as_deref() != Some(id.as_str()) {
                    return Ok(());
                }

                if let Some(error) = error {
                    self.session.fail_attempt(key, false)?;
                    self.error = Some(error.clone());
                } else {
                    let work = work_status(host.owner.session().read(cx));

                    self.session.complete_reply(key, id, text.clone(), work)?;
                }
            }

            ExecutionSignal::Decision { request, .. } => {
                let execution = host.owner.session().clone();
                let mut accepted = false;
                let mut failure = None;

                if attempt.provider_turn.as_deref() == Some(request.provider_turn.as_str()) {
                    if let Ok(arguments) =
                        serde_json::from_value::<DecisionArguments>(request.arguments.clone())
                    {
                        let action = match arguments.action {
                            DecisionAction::Invite => Some(ModeratorAction::Invite {
                                recipients: arguments.recipients,
                            }),

                            DecisionAction::Report if arguments.recipients.is_empty() => {
                                Some(ModeratorAction::Report)
                            }

                            DecisionAction::Report => None,
                        };

                        if let Some(action) = action {
                            match self.session.moderator_decision(
                                key,
                                arguments.stage,
                                arguments.operation,
                                action,
                            ) {
                                Ok(result) => accepted = result,
                                Err(error) => failure = Some(error),
                            }
                        }
                    }

                    if !accepted
                        && let BudgetScope::Discussion(discussion) = attempt.intent.budget
                        && let Err(error) = self.session.pause_discussion(
                            discussion,
                            PauseReason::InvalidModeration(attempt.intent.operation),
                        )
                    {
                        failure = Some(error);
                    }
                }

                execution.update(cx, |session, cx| {
                    if let Some(backend) = session.controller.borrow_mut().runtime.backend_mut() {
                        backend.respond_team_decision(request, accepted, if accepted { "The decision is saved. It will run after this moderator turn finishes." } else { "The decision was rejected. The discussion is paused for user review." });
                    }

                    cx.notify();
                });

                if let Some(error) = failure {
                    return Err(error);
                }
            }
        }

        Ok(())
    }
}
