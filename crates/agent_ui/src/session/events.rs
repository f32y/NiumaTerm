use gpui::Context;
use nmt_agent::AgentEventKind;
use nmt_agent::background_task::BackgroundTaskSnapshot;
use nmt_agent::chat::{
    Event as SessionEvent, Item as SessionItem, QueuedPrompt, ReplayTurn, SessionSummary,
    SlashCommandOutcome, ThreadSettings, TurnActivity,
};
use nmt_agent::session::branch::BranchReplay;
use nmt_agent::session::restore::{ReadyAction, ReplayAction};
#[cfg(test)]
pub(super) use nmt_agent::session::settings::resolve_ready_settings;
use nmt_i18n::i18n;
use tracing::info;

use crate::capabilities::AgentCapabilities as _;
use crate::composer::CommandFeedbackKind;
use crate::session::{Backend, RecoverySnapshot, Status};
use crate::thread_controls::{launch_effort, launch_model, stored_thread_settings};
use crate::transcript::hidden;
use crate::{AgentPane, AgentPaneEvent, RecentSessionsMode};

impl AgentPane {
    /// Apply one typed session event to the transcript and status line.
    pub(crate) fn apply_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            // The pane does not know which tab holds it, so naming the tab is
            // left to the chrome that does. Arriving here is what settles the
            // conversation's name: until then every message asks again.
            SessionEvent::TitleUpdated(title) => {
                self.naming.named = true;
                cx.emit(AgentPaneEvent::TitleSuggested(title));
            }
            SessionEvent::Ready(settings) => self.on_ready(settings, cx),
            SessionEvent::Models(models) => {
                self.controls.state.models = models;
                cx.notify();
            }
            SessionEvent::ApprovalPresets { presets, current } => {
                // The harness owns this control: it reports the presets its
                // deployment serves and which one is in force, so a remembered
                // pick has no say and the row shows what actually applies.
                self.controls.state.approval_presets = presets;
                self.controls.state.settings.approval = current;
                cx.notify();
            }
            SessionEvent::AgentPresets { presets, current } => {
                // The composition is the harness's to report: it is fixed when
                // the conversation is created, and a resumed one carries
                // whichever preset built it rather than whichever this tab last
                // showed.
                self.controls.state.agent_presets = presets;
                self.controls.state.agent_preset = current;
                cx.notify();
            }
            SessionEvent::Commands(commands) => {
                self.palette.provider_commands = commands;
                self.palette.catalog = None;
                self.palette.provider_commands_ready = true;
                self.palette.selected = 0;
                cx.notify();
            }
            SessionEvent::Skills(catalog) => {
                self.palette.skill_catalog = Some(catalog);
                self.palette.selected = 0;
                cx.notify();
            }
            SessionEvent::SlashCommandResult { name, outcome } => {
                self.on_slash_command_result(&name, outcome, cx)
            }
            SessionEvent::TurnStarted => self.on_turn_started(cx),
            SessionEvent::TurnCompleted { error } => self.on_turn_completed(error, cx),
            SessionEvent::TurnOutputTokensUpdated(output_tokens) => {
                self.transcript.update(cx, |transcript, cx| {
                    transcript.set_working_output_tokens(output_tokens, cx)
                });
                cx.notify();
            }
            SessionEvent::ContextWindowUpdated(usage) => {
                self.context_window_usage = Some(usage);
                cx.notify();
            }
            SessionEvent::CompactionStarted => {
                self.note_visible_output();
                self.transcript
                    .update(cx, |transcript, cx| transcript.set_compacting(true, cx));
                cx.notify();
            }
            SessionEvent::CompactionFinished { error } => {
                self.transcript
                    .update(cx, |transcript, cx| transcript.set_compacting(false, cx));

                // A failed compaction is not the turn's own failure, so it needs
                // its own row: the turn continues (and usually then dies on an
                // over-length prompt) with no other trace of why.
                if let Some(text) = error {
                    self.push_item(SessionItem::Error { text }, cx);
                }

                cx.notify();
            }
            SessionEvent::ItemStarted(item) => self.start_item(item, cx),
            SessionEvent::ItemCompleted(item) => self.complete_item(item, cx),
            SessionEvent::AgentMessageDelta { item_id, delta } => {
                self.append_delta(
                    &item_id,
                    &delta,
                    |item| match item {
                        SessionItem::AgentMessage { text, .. } => Some(text),
                        _ => None,
                    },
                    cx,
                );
            }
            SessionEvent::ReasoningSummaryDelta { item_id, delta } => {
                self.append_delta(
                    &item_id,
                    &delta,
                    |item| match item {
                        SessionItem::Reasoning { summary, .. } => Some(summary),
                        _ => None,
                    },
                    cx,
                );
            }
            SessionEvent::CommandOutputDelta { item_id, delta } => {
                self.append_delta(
                    &item_id,
                    &delta,
                    |item| match item {
                        SessionItem::CommandExecution {
                            aggregated_output, ..
                        } => Some(aggregated_output),
                        _ => None,
                    },
                    cx,
                );
            }
            SessionEvent::ApprovalRequested { description } => {
                self.note_visible_output();
                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &i18n("agent-session-needs-input").replace("{name}", self.kind.display()),
                    &description,
                    cx,
                );
                self.prompts.core.ask_approval(description);
                cx.notify();
            }
            SessionEvent::ApprovalResolved => {
                if self.prompts.core.resolve_approval(self.runtime.epoch()) {
                    self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                    cx.notify();
                }
            }
            SessionEvent::QuestionsRequested { questions } => {
                self.note_visible_output();

                // The turn is blocked on the user exactly as an approval is, so
                // it raises the same attention signal rather than a new one.
                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &i18n("agent-session-needs-input").replace("{name}", self.kind.display()),
                    questions
                        .first()
                        .map_or("", |question| question.question.as_str()),
                    cx,
                );
                self.prompts.ask_questions(questions);
                cx.notify();
            }
            SessionEvent::Workflows(snapshot) => {
                self.apply_workflow_snapshot(snapshot, cx);
            }
            SessionEvent::WorkflowAgentTranscript {
                task_id,
                agent_id,
                items,
            } => {
                self.apply_workflow_transcript(&task_id, &agent_id, items, cx);
            }
            SessionEvent::InputRequested(request) => self.receive_questions(request, cx),
            SessionEvent::InputResolved { id, resolution } => {
                self.resolve_questions(&id, resolution, cx)
            }
            SessionEvent::InputSubmissionFailed { id, message } => {
                self.question_submission_failed(&id, message, cx)
            }
            SessionEvent::QuestionsResolved => {
                if self.prompts.core.resolve_legacy(self.runtime.epoch()) {
                    self.prompts.hide_settled();
                    self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                    cx.notify();
                }
            }
            SessionEvent::FileRewindCompleted { error } => {
                let update = self
                    .branch
                    .core
                    .files_completed(self.runtime.epoch(), error.map_or(Ok(()), Err));
                self.apply_rewind_update(update, cx);
            }
            SessionEvent::Error { message, fatal } => self.on_error(message, fatal, cx),
            SessionEvent::EffortRejected { message, effort } => {
                // The pick did not take, so the control returns to the level
                // the session is on. The reason goes to the feedback strip
                // above the composer: it answers for the control the user just
                // used, and the transcript is what the conversation said.
                self.controls.state.settings.effort = effort;
                self.controls
                    .remember_defaults(self.kind, &self.profile, cx);
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
            }
            SessionEvent::History(sessions) => self.on_history(sessions, cx),
            SessionEvent::SessionSearchResults(results) => self.show_search_results(results, cx),
            SessionEvent::ContextCompositionUpdated(composition) => {
                self.context_composition = Some(composition);
                cx.notify();
            }
            SessionEvent::BackgroundTaskTranscript { key, update } => {
                // A child's conversation is view content only: it never
                // reaches the parent transcript, composer, or turn state.
                if update.apply_to(self.children.transcripts.entry(key).or_default()) {
                    cx.notify();
                }
            }
            SessionEvent::BackgroundTasks(snapshot) => self.on_background_tasks(snapshot, cx),
            SessionEvent::QueuedPrompts(prompts) => self.on_queued_prompts(prompts, cx),
            SessionEvent::GoalUpdated(goal) => {
                self.session_state.set_goal(goal);
                cx.notify();
            }
            SessionEvent::PlanModeUpdated(active) => {
                self.session_state.set_plan_mode(active);
                cx.notify();
            }
            SessionEvent::SessionStatsUpdated(stats) => {
                self.session_stats = Some(stats);
                cx.notify();
            }
            SessionEvent::Replay(items) => {
                match self.branch.core.replayed(self.runtime.epoch()) {
                    BranchReplay::Ignore => return,
                    BranchReplay::Unrelated => {}
                    BranchReplay::Complete(completion) => self.complete_branch(completion, cx),
                }
                let resumed = match self.restore.replayed(self.runtime.epoch()) {
                    ReplayAction::Ignore => return,
                    ReplayAction::Append => false,
                    ReplayAction::Replace => true,
                };
                if resumed || self.history_ui.mode == RecentSessionsMode::Loading {
                    self.clear_conversation_presentation(cx);
                    self.history_ui.mode = RecentSessionsMode::Hidden;
                    self.palette.feedback = None;
                }

                self.apply_replay(items, cx);
            }
            SessionEvent::StatusDetail(detail) => self.on_status_detail(detail, cx),
            SessionEvent::ForkCheckpoints(checkpoints) => {
                self.show_fork_checkpoints(checkpoints, cx)
            }
            SessionEvent::HostExited { message } => self.on_host_exited(message, cx),
        }
    }

    fn on_host_exited(&mut self, message: String, cx: &mut Context<Self>) {
        let identity = self.runtime.backend().and_then(Backend::recovery_identity);

        self.runtime.reconnect(Some(RecoverySnapshot {
            identity,
            profile_name: self.profile.name.clone(),
        }));
        self.runtime.recovery_failed(message.clone());
        self.on_error(message, true, cx);
    }

    /// Handshake finished. Fold the reported thread settings together with
    /// remembered picks, settle status, and rebuild child state from history.
    fn on_ready(&mut self, settings: ThreadSettings, cx: &mut Context<Self>) {
        if let Some(completion) = self.branch.core.ready(self.runtime.epoch()) {
            self.complete_branch(completion, cx);
        }
        match self.restore.ready(self.runtime.epoch()) {
            ReadyAction::Ignore => return,
            ReadyAction::Apply => {}
            ReadyAction::Replay(replay) => {
                self.clear_conversation_presentation(cx);
                self.history_ui.mode = RecentSessionsMode::Hidden;
                self.palette.feedback = None;
                self.apply_replay(replay, cx);
            }
        }

        self.restore_question_drafts();

        // Seed the settings dropdowns with the thread's effective
        // configuration so they show real values before any change.
        // Ready can fire again mid-session (Claude's first-turn init
        // confirms the permission mode); a payload without effort
        // keeps the user's pick — Claude never reports effort, so
        // None there means "unknown", never "reset".
        let stored = (self.controls.state.seed_thread_defaults
            || self.controls.state.seed_approval_reviewer)
            .then(|| stored_thread_settings(self.kind, &self.profile, cx))
            .flatten();
        let model = self
            .controls
            .state
            .seed_thread_defaults
            .then(|| launch_model(self.kind, &self.profile))
            .flatten();
        let effort = self
            .controls
            .state
            .seed_thread_defaults
            .then(|| launch_effort(&self.profile))
            .flatten();
        self.controls.state.ready(
            self.kind,
            settings,
            stored,
            model.as_deref(),
            effort.as_deref(),
        );

        // Seeding only fills in the pickers. Where the harness adopts a
        // model through its own request, a remembered or profile pick
        // still has to be pushed, or the row would name a model the
        // session was never switched to.
        if self.kind.caps().model_selection_is_a_request {
            self.apply_model_selection(cx);
        }

        self.sync_pending_rename();
        info!(
            "agent thread ready: profile=\"{}\", model={:?}, profile_model={:?}",
            self.profile.name,
            self.controls.state.settings.model,
            launch_model(self.kind, &self.profile)
        );
        self.runtime.ready();

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
        cx: &mut Context<Self>,
    ) {
        let advance = self
            .palette
            .commands
            .settle(&outcome, self.runtime.status());
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
                self.palette.commands.awaiting_turn = false;
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
    fn on_turn_started(&mut self, cx: &mut Context<Self>) {
        let command_turn = self.palette.commands.turn_started();
        let new_turn = if command_turn {
            self.delivery.begin_turn();
            true
        } else {
            self.delivery.provider_started()
        };

        if new_turn {
            self.start_working(cx);
        }
        self.publish_queued_user_messages(cx);

        self.runtime.turn_started();
        self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
        cx.notify();
    }

    /// Interruption is a completion state of the turn: the stop request
    /// recorded at press time becomes the transcript mark only once the
    /// backend actually ended the turn, so a backend that keeps streaming
    /// never shows an "Interrupted" row above live output. A stale request
    /// for an earlier turn is dropped at this boundary.
    fn on_turn_completed(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        if self.runtime.turn_completed(self.delivery.turn()) {
            let turn = self.delivery.turn();
            self.transcript
                .update(cx, |transcript, _| transcript.mark_interrupted(turn));
        }

        let interrupted_by_user = self
            .transcript
            .read(cx)
            .was_interrupted(self.delivery.turn());
        let error_already_shown = error.as_deref().is_some_and(|text| {
            self.transcript
                .read(cx)
                .turn_has_error(self.delivery.turn(), text)
        });

        let completion_body = error
            .clone()
            .or_else(|| self.latest_agent_message(cx))
            .unwrap_or_else(|| {
                i18n("agent-session-turn-completed").replace("{name}", self.kind.display())
            });

        self.palette.commands.turn_completed();
        self.delivery.completed();

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
    fn on_error(&mut self, message: String, fatal: bool, cx: &mut Context<Self>) {
        self.note_visible_output();

        let branch_failure = self.branch.core.failed(&mut self.runtime, message.clone());
        let resume_failed = self.restore.failed(&mut self.runtime);
        if resume_failed || self.history_ui.mode == RecentSessionsMode::Loading {
            self.history_ui.mode = RecentSessionsMode::Open;

            if !fatal && !resume_failed && branch_failure.is_none() {
                self.runtime.conversation_change_rejected(Status::Idle);
            }

            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-open-failed").replace("{error}", &message),
                cx,
            );
        }

        if let Some(failure) = branch_failure {
            self.report_branch_failure(failure, cx);
        }

        let cancelled_queue = fatal && !self.palette.commands.queue.is_empty();

        if fatal {
            self.prompts.core.disconnect();
            self.prompts.release_secret_editors();

            cx.emit(AgentPaneEvent::Interrupted);
            self.runtime.exited(&message);
            self.delivery.exited();
            self.palette.commands.turn_completed();
            self.palette.commands.queue.clear();
            self.publish_queued_user_messages(cx);
        } else if self.palette.commands.awaiting_turn {
            self.palette.commands.turn_completed();
        }

        self.push_item(SessionItem::Error { text: message }, cx);

        if cancelled_queue {
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

    /// Child lifecycle is reduced by the adapter, so this replaces the pane's
    /// copy without touching the composer, transcript, approval, queued
    /// commands, or running state.
    fn on_background_tasks(&mut self, snapshot: BackgroundTaskSnapshot, cx: &mut Context<Self>) {
        let before = (
            self.background_task_count(),
            self.running_background_tasks(),
        );

        self.children.background_tasks = Some(snapshot);

        // The chrome reveals its control on this tab's first child and
        // then carries the running count, so it is told when either
        // number moves rather than on every refreshed snapshot. A child
        // that is created and finishes within one batch of provider
        // messages never moves the running count, but it does move the
        // total, and it is still a child the view can open.
        if (
            self.background_task_count(),
            self.running_background_tasks(),
        ) != before
        {
            cx.emit(AgentPaneEvent::BackgroundTaskActivity);
        }

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
    fn on_queued_prompts(&mut self, prompts: Vec<QueuedPrompt>, cx: &mut Context<Self>) {
        for text in self.delivery.snapshot(prompts) {
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
            let id = self.delivery.replay_turn();
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
                && let Some(text) = self.delivery.echoed(text)
            {
                self.push_item(SessionItem::UserMessage { text: Some(text) }, cx);
            }
            return;
        }

        if !hidden(&item) {
            self.note_visible_output();
        }

        if matches!(item, SessionItem::AgentMessage { .. }) {
            self.delivery.agent_message();
            self.publish_queued_user_messages(cx);
        }

        self.push_item(item, cx);
    }

    pub(super) fn publish_queued_user_messages(&mut self, cx: &mut Context<Self>) {
        while let Some(text) = self.delivery.pop_confirmed() {
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

    /// Append streamed text to the item `select` picks out. A delta that
    /// actually landed is visible agent output, which resets the idle clock.
    pub(crate) fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        select: fn(&mut SessionItem) -> Option<&mut Option<String>>,
        cx: &mut Context<Self>,
    ) {
        let visible = self.transcript.update(cx, |transcript, _| {
            transcript.append_delta(item_id, delta, select)
        });

        if visible {
            self.note_visible_output();
        }

        cx.notify();
    }
}
