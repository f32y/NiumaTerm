use gpui::Context;
use nmt_agent::AgentEventKind;
use nmt_agent::chat::{
    Event as SessionEvent, Item as SessionItem, ReplayTurn, SessionSummary, SlashCommandOutcome,
    TurnActivity,
};
use nmt_agent::session::controller::{SessionEffect, SessionFailure, SessionReady};
#[cfg(test)]
pub(super) use nmt_agent::session::settings::resolve_ready_settings;
use nmt_agent::transcript::TextField;
use nmt_i18n::i18n;
use tracing::info;

use crate::composer::CommandFeedbackKind;
use crate::session::{Backend, RecoverySnapshot, Status};
use crate::thread_controls::{launch_effort, launch_model, stored_thread_settings};
use crate::transcript::hidden;
use crate::{AgentPane, AgentPaneEvent, RecentSessionsMode};

impl AgentPane {
    /// Apply provider state before presenting its effects in transcript order.
    pub(crate) fn apply_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        let effect = self
            .session
            .apply_event(self.session.runtime.epoch(), event);

        self.present_session_effect(effect, cx);
    }

    pub(super) fn present_session_effect(&mut self, effect: SessionEffect, cx: &mut Context<Self>) {
        match effect {
            SessionEffect::Unchanged => {}
            SessionEffect::Changed => cx.notify(),
            SessionEffect::Title(title) => cx.emit(AgentPaneEvent::TitleSuggested(title)),
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

            SessionEffect::OutputTokens(tokens) => {
                self.transcript.update(cx, |transcript, cx| {
                    transcript.set_working_output_tokens(tokens, cx)
                });

                cx.notify();
            }

            SessionEffect::ContextWindow(usage) => {
                self.context_window_usage = Some(usage);

                cx.notify();
            }

            SessionEffect::ContextComposition(composition) => {
                self.context_composition = Some(composition);

                cx.notify();
            }

            SessionEffect::CompactionStarted => {
                self.note_visible_output();

                self.transcript
                    .update(cx, |transcript, cx| transcript.set_compacting(true, cx));

                cx.notify();
            }

            SessionEffect::CompactionFinished { error } => {
                self.transcript
                    .update(cx, |transcript, cx| transcript.set_compacting(false, cx));

                if let Some(text) = error {
                    self.push_item(SessionItem::Error { text }, cx);
                }

                cx.notify();
            }

            SessionEffect::ItemStarted(item) => self.start_item(item, cx),
            SessionEffect::ItemCompleted(item) => self.complete_item(item, cx),

            SessionEffect::TextDelta {
                item_id,
                delta,
                field,
            } => self.append_delta(&item_id, &delta, field, cx),

            SessionEffect::ApprovalRequested => {
                self.note_visible_output();

                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &i18n("agent-session-needs-input").replace("{name}", self.kind.display()),
                    self.session.input.approval().unwrap_or_default(),
                    cx,
                );

                cx.notify();
            }

            SessionEffect::ApprovalResolved => {
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

                cx.notify();
            }

            SessionEffect::QuestionsRequested { index } => {
                self.note_visible_output();

                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &i18n("agent-session-needs-input").replace("{name}", self.kind.display()),
                    self.session.input.batches()[index]
                        .questions()
                        .first()
                        .map_or("", |question| question.question.as_str()),
                    cx,
                );

                self.prompts.reveal(&self.session.input, index);

                cx.notify();
            }

            SessionEffect::QuestionsResolved => {
                self.prompts.hide_settled(&self.session.input);
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

                cx.notify();
            }

            SessionEffect::InputRequested { index } => self.present_questions(index, cx),

            SessionEffect::InputResolved(completion) => {
                self.present_question_completion(completion, cx)
            }

            SessionEffect::Workflows { activity_changed } => {
                if activity_changed {
                    cx.emit(AgentPaneEvent::WorkflowActivity);
                }

                self.sync_workflow_refresh(cx);

                cx.notify();
            }

            SessionEffect::BackgroundActivity => {
                cx.emit(AgentPaneEvent::BackgroundTaskActivity);
                cx.notify();
            }

            SessionEffect::Branch(update) => self.apply_rewind_update(update, cx),

            SessionEffect::Error {
                message,
                fatal,
                failure,
            } => self.on_error(message, fatal, failure, cx),

            SessionEffect::EffortRejected { message } => {
                self.controls.remember_defaults(
                    &self.session.controls,
                    self.kind,
                    &self.profile,
                    cx,
                );

                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
            }

            SessionEffect::History(sessions) => self.on_history(sessions, cx),
            SessionEffect::SearchResults(results) => self.show_search_results(results, cx),
            SessionEffect::ConfirmedPrompts(prompts) => self.publish_prompts(prompts, cx),

            SessionEffect::Goal(goal) => {
                self.session_state.set_goal(goal);

                cx.notify();
            }

            SessionEffect::PlanMode(active) => {
                self.session_state.set_plan_mode(active);

                cx.notify();
            }

            SessionEffect::Stats(stats) => {
                self.session_stats = Some(stats);

                cx.notify();
            }

            SessionEffect::Replay(replay) => {
                if let Some(completion) = replay.branch {
                    self.complete_branch(completion, cx);
                }

                if replay.replace || self.history_ui.mode == RecentSessionsMode::Loading {
                    if !replay.replace {
                        self.session.clear_conversation();
                    }

                    self.clear_conversation_presentation(cx);
                    self.history_ui.mode = RecentSessionsMode::Hidden;
                    self.palette.feedback = None;
                }

                self.apply_replay(replay.turns, cx);
            }

            SessionEffect::StatusDetail(detail) => self.on_status_detail(detail, cx),

            SessionEffect::ForkCheckpoints(checkpoints) => {
                self.show_fork_checkpoints(checkpoints, cx)
            }

            SessionEffect::HostExited { message } => self.on_host_exited(message, cx),
        }
    }

    fn on_host_exited(&mut self, message: String, cx: &mut Context<Self>) {
        let identity = self
            .session
            .runtime
            .backend()
            .and_then(Backend::recovery_identity);

        self.session.runtime.reconnect(Some(RecoverySnapshot {
            identity,
            profile_name: self.profile.name.clone(),
        }));

        self.session.runtime.recovery_failed(message.clone());

        let failure = self.session.failed(&message, true);

        self.on_error(message, true, failure, cx);
    }

    /// Handshake finished. Fold the reported thread settings together with
    /// remembered picks, settle status, and rebuild child state from history.
    fn on_ready(&mut self, ready: SessionReady, cx: &mut Context<Self>) {
        if let Some(completion) = ready.branch {
            self.complete_branch(completion, cx);
        }

        if let Some(replay) = ready.replay {
            self.clear_conversation_presentation(cx);
            self.history_ui.mode = RecentSessionsMode::Hidden;
            self.palette.feedback = None;
            self.apply_replay(replay, cx);
        }

        // Seed the settings dropdowns with the thread's effective
        // configuration so they show real values before any change.
        // Ready can fire again mid-session (Claude's first-turn init
        // confirms the permission mode); a payload without effort
        // keeps the user's pick — Claude never reports effort, so
        // None there means "unknown", never "reset".
        let stored = (self.session.controls.seed_thread_defaults
            || self.session.controls.seed_approval_reviewer)
            .then(|| stored_thread_settings(self.kind, &self.profile, cx))
            .flatten();

        let model = self
            .session
            .controls
            .seed_thread_defaults
            .then(|| launch_model(self.kind, &self.profile))
            .flatten();

        let effort = self
            .session
            .controls
            .seed_thread_defaults
            .then(|| launch_effort(&self.profile))
            .flatten();

        let selection = self.session.finish_ready(
            self.kind,
            ready.settings,
            stored,
            model.as_deref(),
            effort.as_deref(),
        );

        self.prompts.reset_editors();

        if let Some(Err(error)) = selection {
            self.palette
                .set_feedback(CommandFeedbackKind::Error, error, cx);
        }

        info!(
            "agent thread ready: profile=\"{}\", model={:?}, profile_model={:?}",
            self.profile.name,
            self.session.controls.settings.model,
            launch_model(self.kind, &self.profile)
        );

        // The session id is known by now, so child agents that ran
        // before this tab opened can be rebuilt from history.
        self.restore_background_tasks(cx);
        self.restore_workflows(cx);

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
                    i18n("agent-session-command-accepted").replace("{name}", name),
                    cx,
                );
            }

            SlashCommandOutcome::Completed { message } => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    message.unwrap_or_else(|| {
                        i18n("agent-session-command-completed").replace("{name}", name)
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
                    i18n("agent-session-provider-not-ready").replace("{name}", self.kind.display()),
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
        interrupted: bool,
        cx: &mut Context<Self>,
    ) {
        if interrupted {
            let turn = self.session.delivery.turn();

            self.transcript
                .update(cx, |transcript, _| transcript.mark_interrupted(turn));
        }

        let interrupted_by_user = self
            .transcript
            .read(cx)
            .was_interrupted(self.session.delivery.turn());

        let error_already_shown = error.as_deref().is_some_and(|text| {
            self.transcript
                .read(cx)
                .turn_has_error(self.session.delivery.turn(), text)
        });

        let completion_body = error
            .clone()
            .or_else(|| self.latest_agent_message(cx))
            .unwrap_or_else(|| {
                i18n("agent-session-turn-completed").replace("{name}", self.kind.display())
            });

        // Compaction lives inside a turn; a flag surviving the turn
        // would leave the indicator spinning with nothing behind it.
        self.transcript
            .update(cx, |transcript, cx| transcript.set_compacting(false, cx));

        self.publish_queued_user_messages(cx);

        self.finish_working(cx);
        self.refresh_git_branch(cx);

        if let Some(text) = error
            && !interrupted_by_user
            && !error_already_shown
        {
            self.push_item(SessionItem::Error { text }, cx);
        }

        self.emit_lifecycle(
            AgentEventKind::Stopped,
            &i18n("agent-session-provider-finished").replace("{name}", self.kind.display()),
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
        self.note_visible_output();

        let resume_failed = failure.resume_failed;

        if resume_failed || self.history_ui.mode == RecentSessionsMode::Loading {
            self.history_ui.mode = RecentSessionsMode::Open;

            if !fatal && !resume_failed && failure.branch.is_none() {
                self.session
                    .runtime
                    .conversation_change_rejected(Status::Idle);
            }

            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-open-failed").replace("{error}", &message),
                cx,
            );
        }

        if let Some(failure) = failure.branch {
            self.report_branch_failure(failure, cx);
        }

        if fatal {
            self.prompts.release_secret_editors(&self.session.input);

            cx.emit(AgentPaneEvent::Interrupted);
            self.publish_queued_user_messages(cx);
        }

        self.push_item(SessionItem::Error { text: message }, cx);

        if failure.cancelled_commands {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-queued-cancelled-failed").to_string(),
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

    /// The backend owns its pending inbox, so its snapshot replaces whatever
    /// this side queued optimistically. Anything it dropped is gone from the
    /// list by being absent rather than by a second event saying so, and a
    /// dropped prompt was claimed into the running turn — which is when its
    /// transcript row is due.
    ///
    /// The backend's own echo of that message claims the row too, and either
    /// can arrive first. Both read the same list and remove what they
    /// publish, so whichever loses the race finds nothing left to publish and
    /// the row appears exactly once.
    fn publish_prompts(&mut self, prompts: Vec<String>, cx: &mut Context<Self>) {
        for text in prompts {
            self.push_item(SessionItem::UserMessage { text: Some(text) }, cx);
        }

        cx.notify();
    }

    /// The working row carries it: a turn waiting out a provider retry is
    /// indistinguishable from one thinking slowly, and the elapsed time and
    /// token count say nothing about which it is.
    fn on_status_detail(&mut self, detail: Option<TurnActivity>, cx: &mut Context<Self>) {
        let detail = detail.map(|activity| match activity {
            TurnActivity::Retrying {
                attempt,
                total,
                reason,
            } => i18n("agent-transcript-retrying")
                .replace("{attempt}", &attempt.to_string())
                .replace("{total}", &total.to_string())
                .replace("{reason}", &reason),
        });

        self.transcript.update(cx, |transcript, cx| {
            transcript.set_working_detail(detail, cx)
        });
    }

    /// Pre-fill the transcript with a resumed session's reconstructed
    /// conversation. Replay entries share one turn and carry no fold header,
    /// so they render as a plain chronological stream above the new turns.
    pub(crate) fn apply_replay(&mut self, replay: Vec<ReplayTurn>, cx: &mut Context<Self>) {
        let mut answered_at = None;

        for turn in replay {
            // Each restored turn takes its own id, so the sequence continues
            // past the replay and new turns cannot merge into the last one.
            let id = self.session.delivery.replay_turn();
            let newest = turn.items.iter().filter_map(|item| item.at).max();

            answered_at = answered_at.max(newest);

            self.transcript
                .update(cx, |transcript, cx| transcript.append_replay(id, turn, cx));
        }

        // The restored conversation's idle span runs from the provider's own
        // stamp for its last answer. A transcript that carries no stamps leaves
        // the reading absent, which is all it can honestly say.
        if let Some(at) = answered_at {
            self.note_replayed_response(at, cx);
        }

        cx.notify();
    }

    pub(crate) fn start_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        if let SessionItem::UserMessage { text } = &item {
            if let Some(text) = text
                && let Some(text) = self.session.delivery.echoed(text)
            {
                self.push_item(SessionItem::UserMessage { text: Some(text) }, cx);
            }

            return;
        }

        if !hidden(&item) {
            self.note_visible_output();
        }

        if matches!(item, SessionItem::AgentMessage { .. }) {
            self.session.delivery.agent_message();
            self.publish_queued_user_messages(cx);
        }

        self.push_item(item, cx);
    }

    pub(super) fn publish_queued_user_messages(&mut self, cx: &mut Context<Self>) {
        while let Some(text) = self.session.delivery.pop_confirmed() {
            self.push_item(SessionItem::UserMessage { text: Some(text) }, cx);
        }
    }

    pub(crate) fn complete_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        let Some(id) = item.id().map(str::to_owned) else {
            return;
        };

        let known = self.transcript.read(cx).contains_item(&id);

        // A completed item this pane never saw start (e.g. joined mid-turn)
        // still gets a transcript entry.
        if !known {
            self.start_item(item.clone(), cx);
        }

        self.transcript
            .update(cx, |transcript, _| transcript.merge_completed(&item));

        if !hidden(&item) {
            self.note_visible_output();
        }

        cx.notify();
    }

    /// Append streamed text to the requested field. A delta that
    /// actually landed is visible agent output, which resets the idle clock.
    pub(crate) fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        field: TextField,
        cx: &mut Context<Self>,
    ) {
        let visible = self.transcript.update(cx, |transcript, _| {
            transcript.append_delta(item_id, delta, field)
        });

        if visible {
            self.note_visible_output();
        }

        cx.notify();
    }
}
