#[cfg(test)]
pub(super) use nmt_agent::session::settings::resolve_ready_settings;

use gpui::Context;
use nmt_agent::AgentEventKind;
#[cfg(test)]
use nmt_agent::chat::{Event as SessionEvent, Item as SessionItem, ReplayTurn};
use nmt_agent::chat::{SessionSummary, SlashCommandOutcome};
use nmt_agent::session::branch::BranchUpdate;
#[cfg(test)]
use nmt_agent::session::controller::ReadyDefaults;
use nmt_agent::session::controller::{SessionEffect, SessionFailure, SessionReady};
#[cfg(test)]
use nmt_agent::session::restore::SettingsSeed;
#[cfg(test)]
use nmt_agent::transcript::TextField;
use rust_i18n::t;
use tracing::info;

use crate::agent_tab::composer::CommandFeedbackKind;
use crate::agent_tab::session::{Backend, RecoverySnapshot};
use crate::agent_tab::thread_controls::launch_model;
#[cfg(test)]
use crate::agent_tab::thread_controls::{launch_effort, stored_thread_settings};
use crate::agent_tab::{AgentPane, AgentPaneEvent, RecentSessionsMode};

impl AgentPane {
    /// Apply provider state before presenting its effects in transcript order.
    #[cfg(test)]
    pub(crate) fn on_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        self.prepare_ready_defaults(cx);

        let effect = {
            let mut guard = self.session.borrow_mut();
            let state = &mut *guard;

            state.apply_event(state.runtime.epoch(), event)
        };

        self.present_session_effect(effect, cx);
    }

    pub(super) fn present_session_effect(&mut self, effect: SessionEffect, cx: &mut Context<Self>) {
        self.presenting_session_effect = true;

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

        match effect {
            SessionEffect::Unchanged => {}
            SessionEffect::ProviderTurnAccepted { .. } => {}
            SessionEffect::TeamDecision(_) => {}
            SessionEffect::ProviderTurnFinished { .. } => {}
            SessionEffect::Changed => cx.notify(),

            SessionEffect::Title(title) => {
                self.emit_event(AgentPaneEvent::TitleSuggested(title), cx)
            }

            SessionEffect::Ready(settings) => self.on_ready(settings, cx),

            SessionEffect::Commands(commands) => {
                self.palette.provider_commands = commands;
                self.palette.catalog = None;
                self.palette.provider_commands_ready = true;
                self.palette.selected = 0;

                cx.notify();
            }

            SessionEffect::Skills(catalog) => {
                self.palette.skill_catalog = Some(catalog);
                self.palette.selected = 0;

                cx.notify();
            }

            SessionEffect::CommandResult {
                name,
                outcome,
                advance,
            } => {
                self.on_slash_command_result(&name, outcome, advance, cx);
            }

            SessionEffect::TurnStarted { opened } => self.on_turn_started(opened, cx),

            SessionEffect::TurnCompleted { error, interrupted } => {
                self.on_turn_completed(error, interrupted, cx)
            }

            SessionEffect::OutputTokens(_)
            | SessionEffect::ContextWindow(_)
            | SessionEffect::ContextComposition(_)
            | SessionEffect::CompactionStarted
            | SessionEffect::CompactionFinished { .. }
            | SessionEffect::ItemStarted(_)
            | SessionEffect::ItemCompleted(_)
            | SessionEffect::TextDelta { .. }
            | SessionEffect::ConfirmedPrompts(_)
            | SessionEffect::Goal(_)
            | SessionEffect::PlanMode(_)
            | SessionEffect::Stats(_)
            | SessionEffect::StatusDetail(_) => cx.notify(),

            SessionEffect::ApprovalRequested => {
                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &t!("agent-session-needs-input", name = self.kind.display()),
                    self.session.borrow().input.approval().unwrap_or_default(),
                    cx,
                );

                cx.notify();
            }

            SessionEffect::ApprovalResolved => {
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

                cx.notify();
            }

            SessionEffect::QuestionsRequested { index } => {
                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &t!("agent-session-needs-input", name = self.kind.display()),
                    self.session.borrow().input.batches()[index]
                        .questions()
                        .first()
                        .map_or("", |question| question.question.as_str()),
                    cx,
                );

                self.prompts.reveal(&self.session.borrow().input, index);

                cx.notify();
            }

            SessionEffect::QuestionsResolved => {
                self.prompts.hide_settled(&self.session.borrow().input);
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

                cx.notify();
            }

            SessionEffect::InputRequested { index } => self.present_questions(index, cx),

            SessionEffect::InputResolved(completion) => {
                self.present_question_completion(completion, cx)
            }

            SessionEffect::Workflows { activity_changed } => {
                if activity_changed {
                    self.emit_event(AgentPaneEvent::WorkflowActivity, cx);
                }

                cx.notify();
            }

            SessionEffect::BackgroundActivity => {
                self.emit_event(AgentPaneEvent::BackgroundTaskActivity, cx);

                cx.notify();
            }

            SessionEffect::Branch(update @ BranchUpdate::Branching) => {
                self.on_fork_update(update, cx)
            }

            SessionEffect::Branch(update) => self.on_rewind_update(update, cx),

            SessionEffect::Error {
                message,
                fatal,
                failure,
            } => self.on_error(message, fatal, failure, cx),

            SessionEffect::EffortRejected { message } => {
                self.controls.remember_defaults(
                    &self.session.borrow().controls,
                    self.kind,
                    &self.profile,
                    cx,
                );

                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
            }

            SessionEffect::History(sessions) => self.on_history(sessions, cx),
            SessionEffect::SearchResults(results) => self.show_search_results(results, cx),

            SessionEffect::Replay(replay) => {
                if let Some(completion) = replay.branch {
                    self.complete_branch(completion, cx);
                }

                if replay.replace || self.history_ui.mode == RecentSessionsMode::Loading {
                    self.clear_conversation_presentation(cx);
                    self.history_ui.mode = RecentSessionsMode::Hidden;
                    self.palette.feedback = None;
                }

                self.transcript
                    .update(cx, |transcript, _| transcript.sync_content());
            }

            SessionEffect::ForkCheckpoints(checkpoints) => {
                self.show_fork_checkpoints(checkpoints, cx)
            }

            SessionEffect::HostExited { message } => self.on_host_exited(message, cx),
        }

        self.presenting_session_effect = false;
    }

    fn on_host_exited(&mut self, message: String, cx: &mut Context<Self>) {
        let identity = self
            .session
            .borrow()
            .runtime
            .backend()
            .and_then(Backend::recovery_identity);

        self.session
            .borrow_mut()
            .runtime
            .reconnect(Some(RecoverySnapshot {
                identity,
                profile_name: self.profile.name.clone(),
            }));

        self.session
            .borrow_mut()
            .runtime
            .recovery_failed(message.clone());

        let failure = self.session.borrow_mut().failed(&message, true);

        self.on_error(message, true, failure, cx);
    }

    /// Handshake finished. Fold the reported thread settings together with
    /// remembered picks, settle status, and rebuild child state from history.
    fn on_ready(&mut self, ready: SessionReady, cx: &mut Context<Self>) {
        if let Some(completion) = ready.branch {
            self.complete_branch(completion, cx);
        }

        if ready.replaced {
            self.clear_conversation_presentation(cx);
            self.history_ui.mode = RecentSessionsMode::Hidden;
            self.palette.feedback = None;
        }

        let selection = ready.selection;

        self.prompts.reset_editors();

        if let Some(Err(error)) = selection {
            self.palette
                .set_feedback(CommandFeedbackKind::Error, error, cx);
        }

        info!(
            "agent thread ready: profile=\"{}\", model={:?}, profile_model={:?}",
            self.profile.name,
            self.session.borrow().controls.settings.model,
            launch_model(self.kind, &self.profile)
        );

        // The session id is known by now, so child agents that ran
        // before this tab opened can be rebuilt from history.

        cx.notify();
    }

    /// Asynchronous provider acknowledgement for a command request; feedback
    /// goes to the strip above the composer, and a settled command hands the
    /// queue to the next one.
    fn on_slash_command_result(
        &mut self,
        name: &str,
        outcome: SlashCommandOutcome,
        advance: bool,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            SlashCommandOutcome::Accepted => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    t!("agent-session-command-accepted", name = name).into_owned(),
                    cx,
                );
            }

            SlashCommandOutcome::Completed { message } => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    message.unwrap_or_else(|| {
                        t!("agent-session-command-completed", name = name).into_owned()
                    }),
                    cx,
                );
            }

            SlashCommandOutcome::Rejected { message } => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
            }

            SlashCommandOutcome::NotReady => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    t!(
                        "agent-session-provider-not-ready",
                        name = self.kind.display()
                    )
                    .into_owned(),
                    cx,
                );

                self.run_next_queued_command(cx);
            }
        }

        if advance {
            self.run_next_queued_command(cx);
        }
    }

    /// A turn a send opened numbered itself and started its timer at send
    /// time. A command's turn and a turn the harness opened on its own —
    /// running a prompt it held while the last turn finished — both arrive
    /// with neither done, and without them the whole turn would be filed
    /// under the previous one and leave the pane looking idle while it runs.
    fn on_turn_started(&mut self, new_turn: bool, cx: &mut Context<Self>) {
        if new_turn {
            self.start_working(cx);
        }

        self.publish_queued_user_messages(cx);

        self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);

        cx.notify();
    }

    /// Interruption is a completion state of the turn: the stop request
    /// recorded at press time becomes the transcript mark only once the
    /// backend actually ended the turn, so a backend that keeps streaming
    /// never shows an "Interrupted" row above live output. A stale request
    /// for an earlier turn is dropped at this boundary.
    fn on_turn_completed(
        &mut self,
        error: Option<String>,
        _interrupted: bool,
        cx: &mut Context<Self>,
    ) {
        let completion_body = error
            .clone()
            .or_else(|| self.latest_agent_message(cx))
            .unwrap_or_else(|| {
                t!("agent-session-turn-completed", name = self.kind.display()).into_owned()
            });

        self.turn.refresh_timer(cx);
        self.refresh_git_branch(cx);

        self.emit_lifecycle(
            AgentEventKind::Stopped,
            &t!(
                "agent-session-provider-finished",
                name = self.kind.display()
            ),
            &completion_body,
            cx,
        );

        self.run_next_queued_command(cx);

        cx.notify();
    }

    /// A backend error lands in the transcript; a fatal one also ends the
    /// session, returns queued work, and reports the interruption outward.
    fn on_error(
        &mut self,
        message: String,
        fatal: bool,
        failure: SessionFailure,
        cx: &mut Context<Self>,
    ) {
        let resume_failed = failure.resume_failed;

        if resume_failed || self.history_ui.mode == RecentSessionsMode::Loading {
            self.history_ui.mode = RecentSessionsMode::Open;

            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-open-failed", error = &message).into_owned(),
                cx,
            );
        }

        if let Some(failure) = failure.branch {
            self.report_branch_failure(failure, cx);
        }

        if fatal {
            self.prompts
                .release_secret_editors(&self.session.borrow().input);

            self.emit_event(AgentPaneEvent::Interrupted, cx);
            self.publish_queued_user_messages(cx);
        }

        if failure.cancelled_commands {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-queued-cancelled-failed").to_string(),
                cx,
            );
        } else if !fatal {
            self.run_next_queued_command(cx);
        }
    }

    /// A list of what is recent answers a different question than the search
    /// currently on screen, so it replaces those rows rather than being
    /// appended to them.
    fn on_history(&mut self, sessions: Vec<SessionSummary>, cx: &mut Context<Self>) {
        self.history_ui.data.append_page(sessions);

        cx.notify();
    }

    /// Pre-fill the transcript with a resumed session's reconstructed
    /// conversation. Replay entries share one turn and carry no fold header,
    /// so they render as a plain chronological stream above the new turns.
    #[cfg(test)]
    pub(crate) fn on_replay(&mut self, replay: Vec<ReplayTurn>, cx: &mut Context<Self>) {
        let answered_at = replay
            .iter()
            .flat_map(|turn| turn.items.iter())
            .filter_map(|item| item.at)
            .max();

        self.session.borrow_mut().apply_replay(replay);

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

        if let Some(at) = answered_at {
            self.note_replayed_response(at, cx);
        }

        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn prepare_ready_defaults(&mut self, cx: &Context<Self>) {
        let mut session = self.session.borrow_mut();

        session.ready_defaults = match session.controls.seed {
            SettingsSeed::Defaults => ReadyDefaults {
                stored: stored_thread_settings(self.kind, &self.profile, cx).cloned(),
                model: launch_model(self.kind, &self.profile),
                effort: launch_effort(&self.profile),
            },

            SettingsSeed::Reviewer => ReadyDefaults {
                stored: stored_thread_settings(self.kind, &self.profile, cx).cloned(),
                ..ReadyDefaults::default()
            },

            SettingsSeed::None => ReadyDefaults::default(),
        };
    }

    #[cfg(test)]
    pub(crate) fn start_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        self.session.borrow_mut().start_item(item);

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

        cx.notify();
    }

    pub(super) fn publish_queued_user_messages(&mut self, cx: &mut Context<Self>) {
        self.session.borrow_mut().publish_confirmed();

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());
    }

    #[cfg(test)]
    pub(crate) fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        field: TextField,
        cx: &mut Context<Self>,
    ) {
        self.session
            .borrow_mut()
            .append_delta(item_id, delta, field);

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

        cx.notify();
    }
}
