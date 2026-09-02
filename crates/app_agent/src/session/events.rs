use std::mem::take;

use gpui::Context;
use nmt_agent_utils::AgentEventKind;
use nmt_agent_utils::background_task::BackgroundTaskSnapshot;
use nmt_agent_utils::chat::{
    Event as SessionEvent, Item as SessionItem, QueuedPrompt, ReplayTurn, SessionSummary,
    SlashCommandOutcome, ThreadSettings, TurnActivity,
};
use nmt_i18n::i18n;
use tracing::{info, warn};

use crate::capabilities::QueuedPromptDelivery;
use crate::commands::claim_command_turn_start;
use crate::composer::CommandFeedbackKind;
use crate::session::conversation::claimed_prompts;
use crate::session::{Backend, RecoverySnapshot, Status, UpdateSuspension};
use crate::thread_controls::{launch_effort, launch_model, stored_thread_settings};
use crate::transcript::hidden;
use crate::{AgentPane, AgentPaneEvent, QuestionPrompt, RecentSessionsMode};

/// Fold the thread's reported settings together with what the pane
/// remembered. `startup_model` and `startup_effort` come from the launch
/// profile and are applied last, so a profile that pins one of them wins over
/// both the remembered pick and whatever the agent reported.
pub(super) fn resolve_ready_settings(
    mut next: ThreadSettings,
    local: Option<&ThreadSettings>,
    use_all_local: bool,
    use_local_reviewer: bool,
    startup_model: Option<&str>,
    startup_effort: Option<&str>,
) -> ThreadSettings {
    if use_all_local && let Some(local) = local {
        next = ThreadSettings {
            model: local.model.clone().or(next.model),
            approval: local.approval.clone().or(next.approval),
            approvals_reviewer: local.approvals_reviewer.clone().or(next.approvals_reviewer),
            sandbox: local.sandbox.clone().or(next.sandbox),
            effort: local.effort.clone().or(next.effort),
            tier: local.tier.clone().or(next.tier),
        };
    }
    if use_local_reviewer
        && let Some(reviewer) = local.and_then(|local| local.approvals_reviewer.clone())
    {
        next.approvals_reviewer = Some(reviewer);
    }
    if let Some(model) = startup_model {
        next.model = Some(model.to_string());
    }
    if let Some(effort) = startup_effort {
        next.effort = Some(effort.to_string());
    }
    next
}

impl AgentPane {
    /// Apply one typed session event to the transcript and status line.
    pub(crate) fn apply_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            // The pane does not know which tab holds it, so naming the tab is
            // left to the chrome that does. Arriving here is what settles the
            // conversation's name: until then every message asks again.
            SessionEvent::TitleUpdated(title) => {
                self.conversation_named = true;
                cx.emit(AgentPaneEvent::TitleSuggested(title));
            }
            SessionEvent::Ready(settings) => self.on_ready(settings, cx),
            SessionEvent::Models(models) => {
                self.controls.models = models;
                cx.notify();
            }
            SessionEvent::ApprovalPresets { presets, current } => {
                // The harness owns this control: it reports the presets its
                // deployment serves and which one is in force, so a remembered
                // pick has no say and the row shows what actually applies.
                self.controls.approval_presets = presets;
                self.controls.settings.approval = current;
                cx.notify();
            }
            SessionEvent::AgentPresets { presets, current } => {
                // The composition is the harness's to report: it is fixed when
                // the conversation is created, and a resumed one carries
                // whichever preset built it rather than whichever this tab last
                // showed.
                self.controls.agent_presets = presets;
                self.controls.agent_preset = current;
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
                self.note_visible_agent_output();
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
                self.note_visible_agent_output();
                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &i18n("agent-session-needs-input").replace("{name}", self.kind.display()),
                    &description,
                    cx,
                );
                self.pending_approval = Some(description);
                cx.notify();
            }
            SessionEvent::ApprovalResolved => {
                self.pending_approval = None;
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                cx.notify();
            }
            SessionEvent::QuestionsRequested { questions } => {
                self.note_visible_agent_output();
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
                self.pending_questions = Some(QuestionPrompt::new(questions));
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
            SessionEvent::QuestionsResolved => {
                self.pending_questions = None;
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                cx.notify();
            }
            SessionEvent::FileRewindCompleted { error } => {
                let result = error.map_or(Ok(()), Err);
                if let Some(completion) = self.rewind.file_completion.take() {
                    let _ = completion.send(result);
                } else {
                    warn!("received a Claude file rewind result with no pending UI operation");
                }
            }
            SessionEvent::Error { message, fatal } => self.on_error(message, fatal, cx),
            SessionEvent::EffortRejected { message, effort } => {
                // The pick did not take, so the control returns to the level
                // the session is on. The reason goes to the feedback strip
                // above the composer: it answers for the control the user just
                // used, and the transcript is what the conversation said.
                self.controls.settings.effort = effort;
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
                if self.history_ui.mode == RecentSessionsMode::Loading {
                    self.clear_conversation_presentation(cx);
                    self.history_ui.mode = RecentSessionsMode::Hidden;
                    self.palette.feedback = None;
                }
                self.apply_replay(items, cx);
                // A branch is finished once the copy's own history has replaced
                // the transcript, which is the moment it is a conversation the
                // user can type into rather than one still being cut.
                self.finish_conversation_branch(cx);
            }
            SessionEvent::StatusDetail(detail) => self.on_status_detail(detail, cx),
            SessionEvent::ForkCheckpoints(checkpoints) => {
                self.show_fork_checkpoints(checkpoints, cx)
            }
            SessionEvent::HostExited { message } => self.on_host_exited(message, cx),
        }
    }

    fn on_host_exited(&mut self, message: String, cx: &mut Context<Self>) {
        let identity = self
            .runtime
            .backend
            .as_ref()
            .and_then(Backend::recovery_identity);
        self.runtime.last_recovery_snapshot = Some(RecoverySnapshot {
            identity,
            profile_name: self.profile.name.clone(),
        });
        self.runtime.update_suspension = Some(UpdateSuspension::Failed(message.clone()));
        self.on_error(message, true, cx);
    }

    /// Handshake finished. Fold the reported thread settings together with
    /// remembered picks, settle status, and rebuild child state from history.
    fn on_ready(&mut self, settings: ThreadSettings, cx: &mut Context<Self>) {
        if self.history_ui.mode == RecentSessionsMode::Loading
            && let Some(replay) = self.history_ui.pending_resume_replay.take()
        {
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
        let effort = settings
            .effort
            .clone()
            .or(self.controls.settings.effort.clone());
        let mut next = ThreadSettings { effort, ..settings };

        // Fresh conversations, and resumes into a harness that does not
        // replay its own controls, seed all remembered picks. Where
        // another Ready arrives during first-turn initialization, that
        // later confirmation preserves the controls in use instead of
        // restoring the ones the CLI reports.
        let seed_thread_defaults = take(&mut self.controls.seed_thread_defaults);
        let seed_approval_reviewer = take(&mut self.controls.seed_approval_reviewer);
        let stored = (seed_thread_defaults || seed_approval_reviewer)
            .then(|| stored_thread_settings(self.kind, &self.profile, cx))
            .flatten();
        let preserve_current = self.kind.caps().repeats_ready_during_init && !seed_thread_defaults;
        let local = if preserve_current {
            Some(&self.controls.settings)
        } else {
            stored
        };
        let startup_model = seed_thread_defaults
            .then(|| launch_model(self.kind, &self.profile))
            .flatten();
        let startup_effort = seed_thread_defaults
            .then(|| launch_effort(&self.profile))
            .flatten();
        next = resolve_ready_settings(
            next,
            local,
            seed_thread_defaults || preserve_current,
            seed_approval_reviewer,
            startup_model.as_deref(),
            startup_effort.as_deref(),
        );

        if let Some(restored) = self.controls.restore_on_ready.take() {
            next = resolve_ready_settings(next, Some(&restored), true, false, None, None);
        }

        self.controls.settings = next;
        // Seeding only fills in the pickers. Where the harness adopts a
        // model through its own request, a remembered or profile pick
        // still has to be pushed, or the row would name a model the
        // session was never switched to.
        if self.kind.caps().model_selection_is_a_request {
            self.apply_model_selection(cx);
        }
        if let Some(title) = self.pending_conversation_rename.take()
            && let Some(session) = self.runtime.backend.as_mut()
        {
            session.rename_session(&title);
        }
        info!(
            "agent thread ready: profile=\"{}\", model={:?}, profile_model={:?}",
            self.profile.name,
            self.controls.settings.model,
            launch_model(self.kind, &self.profile)
        );
        // Claude's first-turn init confirms settings after its
        // synthetic TurnStarted event; that confirmation must not
        // make an active turn look idle and admit overlapping work.
        if self.runtime.status != Status::Running {
            self.runtime.status = Status::Idle;
        }
        if matches!(
            self.runtime.update_suspension,
            Some(UpdateSuspension::Reconnecting)
        ) {
            self.runtime.update_suspension = None;
        }
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
                if self.palette.awaiting_command_turn && self.runtime.status != Status::Running {
                    self.palette.awaiting_command_turn = false;
                    self.run_next_queued_command(cx);
                }
            }
            SlashCommandOutcome::Rejected { message } => {
                self.palette.awaiting_command_turn = false;
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
                self.run_next_queued_command(cx);
            }
            SlashCommandOutcome::NotReady => {
                self.palette.awaiting_command_turn = false;
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-session-provider-not-ready").replace("{name}", self.kind.display()),
                    cx,
                );
                self.run_next_queued_command(cx);
            }
        }
    }

    /// A turn a send opened numbered itself and started its timer at send
    /// time. A command's turn and a turn the harness opened on its own —
    /// running a prompt it held while the last turn finished — both arrive
    /// with neither done, and without them the whole turn would be filed
    /// under the previous one and leave the pane looking idle while it runs.
    fn on_turn_started(&mut self, cx: &mut Context<Self>) {
        let command_turn = claim_command_turn_start(&mut self.palette.awaiting_command_turn);
        let harness_opened = !command_turn && !self.transcript.read(cx).is_working();

        if command_turn || harness_opened {
            self.turn.seq += 1;
            self.start_working(cx);
        }
        // The prompt the harness held is what this turn answers, so it
        // heads this turn rather than trailing the finished one.
        if harness_opened
            && self.kind.caps().queued_prompt_delivery == QueuedPromptDelivery::FollowingTurn
        {
            self.publish_queued_user_messages(cx);
        }
        self.runtime.status = Status::Running;
        self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
        cx.notify();
    }

    /// Interruption is a completion state of the turn: the stop request
    /// recorded at press time becomes the transcript mark only once the
    /// backend actually ended the turn, so a backend that keeps streaming
    /// never shows an "Interrupted" row above live output. A stale request
    /// for an earlier turn is dropped at this boundary.
    fn on_turn_completed(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        if self.turn.pending_interrupt.take() == Some(self.turn.seq) {
            let turn = self.turn.seq;
            self.transcript
                .update(cx, |transcript, _| transcript.mark_interrupted(turn));
        }
        let interrupted_by_user = self.transcript.read(cx).was_interrupted(self.turn.seq);
        let completion_body = error
            .clone()
            .or_else(|| self.latest_agent_message(cx))
            .unwrap_or_else(|| {
                i18n("agent-session-turn-completed").replace("{name}", self.kind.display())
            });
        self.palette.awaiting_command_turn = false;
        self.turn.unanswered_prompt = None;
        // Compaction lives inside a turn; a flag surviving the turn
        // would leave the indicator spinning with nothing behind it.
        self.transcript
            .update(cx, |transcript, cx| transcript.set_compacting(false, cx));
        // A prompt steered into this turn is one the backend never
        // acknowledges, so the turn's end is the last moment that can
        // still say it went in. The other two deliveries run their
        // queue in a turn of its own, which the end of this one does
        // not make sent.
        if self.kind.caps().queued_prompt_delivery == QueuedPromptDelivery::RunningTurn {
            self.publish_queued_user_messages(cx);
        }
        self.finish_working(cx);
        self.refresh_git_branch(cx);
        if self.runtime.status == Status::Running {
            self.runtime.status = Status::Idle;
        }
        if let Some(text) = error
            && !interrupted_by_user
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
        self.note_visible_agent_output();
        if self.history_ui.mode == RecentSessionsMode::Loading {
            self.history_ui.mode = RecentSessionsMode::Open;
            self.history_ui.pending_resume_replay = None;
            // A branch that never arrives would otherwise hold the
            // composer behind a conversation that is not being cut.
            self.abandon_conversation_branch();
            if !fatal {
                self.runtime.status = Status::Idle;
            }
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-open-failed").replace("{error}", &message),
                cx,
            );
        }
        if fatal
            && matches!(
                self.runtime.update_suspension,
                Some(UpdateSuspension::Reconnecting)
            )
        {
            self.runtime.update_suspension = Some(UpdateSuspension::Failed(message.clone()));
        }
        let cancelled_queue = fatal && !self.palette.command_queue.is_empty();
        if fatal {
            cx.emit(AgentPaneEvent::Interrupted);
            self.runtime.status = Status::Exited;
            self.turn.unanswered_prompt = None;
            self.palette.awaiting_command_turn = false;
            self.palette.command_queue.clear();
            self.publish_queued_user_messages(cx);
        } else if self.palette.awaiting_command_turn {
            self.palette.awaiting_command_turn = false;
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
        if take(&mut self.history_ui.showing_search) {
            self.history_ui.sessions.clear();
        }

        // Pages accumulate: the first page lands in an empty list,
        // later cursor pages extend it. A /new backend may publish
        // the first page again, so ids are deduplicated in place.
        for session in sessions {
            if !self
                .history_ui
                .sessions
                .iter()
                .any(|existing| existing.id == session.id)
            {
                self.history_ui.sessions.push(session);
            }
        }
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
    fn on_queued_prompts(&mut self, mut prompts: Vec<QueuedPrompt>, cx: &mut Context<Self>) {
        // A prompt whose own send started the turn is already in the
        // transcript, and the backend keeps listing it until the turn
        // claims it. Repeating it above the composer would show the
        // same message twice for that whole window, so it is dropped
        // from the list here and the snapshot that stops naming it —
        // the moment the turn took it — retires the record.
        if let Some(drawn) = self.turn.published_prompt.take() {
            let before = prompts.len();
            prompts.retain(|prompt| prompt.text != drawn);
            if prompts.len() != before {
                self.turn.published_prompt = Some(drawn);
            }
        }

        let claimed = claimed_prompts(&self.turn.queued_user_messages, &prompts);

        self.turn.queued_user_messages = prompts.into();
        for text in claimed {
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
            self.turn.seq += 1;
            let id = self.turn.seq;
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
                && self
                    .turn
                    .queued_user_messages
                    .front()
                    .is_some_and(|queued| &queued.text == text)
            {
                let text = text.clone();
                self.turn.queued_user_messages.pop_front();
                self.push_item(SessionItem::UserMessage { text: Some(text) }, cx);
            }
            return;
        }

        if !hidden(&item) {
            self.note_visible_agent_output();
        }

        // Where a prompt joins the turn already in flight, assistant output is
        // the only sign the backend gives that it landed. The other two
        // deliveries answer the question themselves — one by opening a turn
        // for the prompt, one by listing it until a turn claims it — and
        // guessing beside either would show a message as sent while it is
        // still waiting.
        if matches!(item, SessionItem::AgentMessage { .. })
            && self.kind.caps().queued_prompt_delivery == QueuedPromptDelivery::RunningTurn
        {
            self.publish_queued_user_messages(cx);
        }

        self.push_item(item, cx);
    }

    pub(super) fn publish_queued_user_messages(&mut self, cx: &mut Context<Self>) {
        while let Some(queued) = self.turn.queued_user_messages.pop_front() {
            self.push_item(
                SessionItem::UserMessage {
                    text: Some(queued.text),
                },
                cx,
            );
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
            self.note_visible_agent_output();
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
            self.note_visible_agent_output();
        }
        cx.notify();
    }
}
