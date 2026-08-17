use nmt_i18n::i18n;

use crate::agent_pane::composer::CommandFeedbackKind;
use crate::agent_pane::session::{Status, UpdateSuspension};
use crate::agent_pane::transcript::hidden;
use crate::agent_pane::*;

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
    pub(in crate::agent_pane) fn apply_event(
        &mut self,
        event: SessionEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            SessionEvent::Ready(settings) => {
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
                let effort = settings.effort.clone().or(self.settings.effort.clone());
                let mut next = ThreadSettings { effort, ..settings };

                // Fresh conversations, and resumes into a harness that does not
                // replay its own controls, seed all remembered picks. Where
                // another Ready arrives during first-turn initialization, that
                // later confirmation preserves the controls in use instead of
                // restoring the ones the CLI reports.
                let seed_thread_defaults = take(&mut self.seed_thread_defaults);
                let seed_approval_reviewer = take(&mut self.seed_approval_reviewer);
                let stored = (seed_thread_defaults || seed_approval_reviewer)
                    .then(|| self.stored_thread_settings(cx))
                    .flatten();
                let preserve_current =
                    self.kind.caps().repeats_ready_during_init && !seed_thread_defaults;
                let local = if preserve_current {
                    Some(&self.settings)
                } else {
                    stored
                };
                let startup_model = seed_thread_defaults.then(|| self.profile_model()).flatten();
                let startup_effort = seed_thread_defaults
                    .then(|| self.profile_effort())
                    .flatten();
                next = resolve_ready_settings(
                    next,
                    local,
                    seed_thread_defaults || preserve_current,
                    seed_approval_reviewer,
                    startup_model.as_deref(),
                    startup_effort.as_deref(),
                );

                if let Some(restored) = self.restore_thread_settings_on_ready.take() {
                    next = resolve_ready_settings(next, Some(&restored), true, false, None, None);
                }

                self.settings = next;
                // Seeding only fills in the pickers. Where the harness adopts a
                // model through its own request, a remembered or profile pick
                // still has to be pushed, or the row would name a model the
                // session was never switched to.
                if self.kind.caps().model_selection_is_a_request {
                    self.apply_model_selection(cx);
                }
                info!(
                    "agent thread ready: profile=\"{}\", model={:?}, profile_model={:?}",
                    self.profile.name,
                    self.settings.model,
                    self.profile_model()
                );
                // Claude's first-turn init confirms settings after its
                // synthetic TurnStarted event; that confirmation must not
                // make an active turn look idle and admit overlapping work.
                if self.status != Status::Running {
                    self.status = Status::Idle;
                }
                if matches!(self.update_suspension, Some(UpdateSuspension::Reconnecting)) {
                    self.update_suspension = None;
                }
                // The session id is known by now, so child agents that ran
                // before this tab opened can be rebuilt from history.
                self.restore_background_tasks(cx);
                self.restore_workflows(cx);
                cx.notify();
            }
            SessionEvent::Models(models) => {
                self.models = models;
                cx.notify();
            }
            SessionEvent::ApprovalPresets { presets, current } => {
                // The harness owns this control: it reports the presets its
                // deployment serves and which one is in force, so a remembered
                // pick has no say and the row shows what actually applies.
                self.approval_presets = presets;
                self.settings.approval = current;
                cx.notify();
            }
            SessionEvent::Commands(commands) => {
                self.palette.provider_commands = commands;
                self.palette.provider_commands_ready = true;
                self.palette.selected = 0;
                cx.notify();
            }
            SessionEvent::Skills(catalog) => {
                self.palette.skill_catalog = Some(catalog);
                self.palette.selected = 0;
                cx.notify();
            }
            SessionEvent::SlashCommandResult { name, outcome } => match outcome {
                SlashCommandOutcome::Accepted => {
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        i18n("agent-session-command-accepted").replace("{name}", &name),
                        cx,
                    );
                }
                SlashCommandOutcome::Completed { message } => {
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        message.unwrap_or_else(|| {
                            i18n("agent-session-command-completed").replace("{name}", &name)
                        }),
                        cx,
                    );
                    if self.palette.awaiting_command_turn && self.status != Status::Running {
                        self.palette.awaiting_command_turn = false;
                        self.run_next_queued_command(cx);
                    }
                }
                SlashCommandOutcome::Rejected { message } => {
                    self.palette.awaiting_command_turn = false;
                    self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                    self.run_next_queued_command(cx);
                }
                SlashCommandOutcome::NotReady => {
                    self.palette.awaiting_command_turn = false;
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        i18n("agent-session-provider-not-ready")
                            .replace("{name}", self.kind.display()),
                        cx,
                    );
                    self.run_next_queued_command(cx);
                }
            },
            SessionEvent::TurnStarted => {
                if claim_command_turn_start(&mut self.palette.awaiting_command_turn) {
                    self.turn_seq += 1;
                    self.start_working(cx);
                }
                self.status = Status::Running;
                self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
                cx.notify();
            }
            SessionEvent::TurnCompleted { error } => {
                // Interruption is a completion state of the turn: the stop
                // request recorded at press time becomes the transcript mark
                // only once the backend actually ended the turn, so a backend
                // that keeps streaming never shows an "Interrupted" row above
                // live output. A stale request for an earlier turn is dropped
                // at this boundary.
                if self.pending_interrupt.take() == Some(self.turn_seq) {
                    let turn = self.turn_seq;
                    self.transcript
                        .update(cx, |transcript, _| transcript.mark_interrupted(turn));
                }
                let interrupted_by_user = self.transcript.read(cx).was_interrupted(self.turn_seq);
                let completion_body = error
                    .clone()
                    .or_else(|| self.latest_agent_message(cx))
                    .unwrap_or_else(|| {
                        i18n("agent-session-turn-completed").replace("{name}", self.kind.display())
                    });
                self.palette.awaiting_command_turn = false;
                self.unanswered_prompt = None;
                // Compaction lives inside a turn; a flag surviving the turn
                // would leave the indicator spinning with nothing behind it.
                self.transcript
                    .update(cx, |transcript, cx| transcript.set_compacting(false, cx));
                self.publish_queued_user_messages(cx);
                self.finish_working(cx);
                self.refresh_git_branch(cx);
                if self.status == Status::Running {
                    self.status = Status::Idle;
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
                let visible = self.append_delta(
                    &item_id,
                    &delta,
                    |item| match item {
                        SessionItem::AgentMessage { text, .. } => Some(text),
                        _ => None,
                    },
                    cx,
                );
                if visible {
                    self.note_visible_agent_output();
                }
                cx.notify();
            }
            SessionEvent::ReasoningSummaryDelta { item_id, delta } => {
                let visible = self.append_delta(
                    &item_id,
                    &delta,
                    |item| match item {
                        SessionItem::Reasoning { summary, .. } => Some(summary),
                        _ => None,
                    },
                    cx,
                );
                if visible {
                    self.note_visible_agent_output();
                }
                cx.notify();
            }
            SessionEvent::CommandOutputDelta { item_id, delta } => {
                let visible = self.append_delta(
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
                if visible {
                    self.note_visible_agent_output();
                }
                cx.notify();
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
            SessionEvent::Error { message, fatal } => {
                self.note_visible_agent_output();
                if self.history_ui.mode == RecentSessionsMode::Loading {
                    self.history_ui.mode = RecentSessionsMode::Open;
                    self.history_ui.pending_resume_replay = None;
                    if !fatal {
                        self.status = Status::Idle;
                    }
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        i18n("agent-session-open-failed").replace("{error}", &message),
                        cx,
                    );
                }
                if fatal && matches!(self.update_suspension, Some(UpdateSuspension::Reconnecting)) {
                    self.update_suspension = Some(UpdateSuspension::Failed(message.clone()));
                }
                let cancelled_queue = fatal && !self.palette.command_queue.is_empty();
                if fatal {
                    cx.emit(AgentPaneEvent::Interrupted);
                    self.status = Status::Exited;
                    self.unanswered_prompt = None;
                    self.palette.awaiting_command_turn = false;
                    self.palette.command_queue.clear();
                    self.publish_queued_user_messages(cx);
                } else if self.palette.awaiting_command_turn {
                    self.palette.awaiting_command_turn = false;
                }
                self.push_item(SessionItem::Error { text: message }, cx);
                if cancelled_queue {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        i18n("agent-session-queued-cancelled-failed").to_string(),
                        cx,
                    );
                } else if !fatal {
                    self.run_next_queued_command(cx);
                }
            }
            SessionEvent::History(sessions) => {
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
            SessionEvent::ContextCompositionUpdated(composition) => {
                self.context_composition = Some(composition);
                cx.notify();
            }
            SessionEvent::BackgroundTaskTranscript { key, update } => {
                // A child's conversation is view content only: it never
                // reaches the parent transcript, composer, or turn state.
                if update.apply_to(self.background_task_transcripts.entry(key).or_default()) {
                    cx.notify();
                }
            }
            SessionEvent::BackgroundTasks(snapshot) => {
                // Child lifecycle is reduced by the adapter, so this replaces
                // the pane's copy without touching the composer, transcript,
                // approval, queued commands, or running state.
                let before = (
                    self.background_task_count(),
                    self.running_background_tasks(),
                );
                self.background_tasks = Some(snapshot);
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
            SessionEvent::Replay(items) => {
                if self.history_ui.mode == RecentSessionsMode::Loading {
                    self.clear_conversation_presentation(cx);
                    self.history_ui.mode = RecentSessionsMode::Hidden;
                    self.palette.feedback = None;
                }
                self.apply_replay(items, cx);
            }
            // The working row carries it: a turn waiting out a provider retry
            // is indistinguishable from one thinking slowly, and the elapsed
            // time and token count say nothing about which it is.
            SessionEvent::StatusDetail(detail) => {
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
        }
    }

    /// Pre-fill the transcript with a resumed session's reconstructed
    /// conversation. Replay entries share one turn and carry no fold header,
    /// so they render as a plain chronological stream above the new turns.
    pub(in crate::agent_pane) fn apply_replay(
        &mut self,
        replay: Vec<ReplayTurn>,
        cx: &mut Context<Self>,
    ) {
        for turn in replay {
            // Each restored turn takes its own id, so the sequence continues
            // past the replay and new turns cannot merge into the last one.
            self.turn_seq += 1;
            let id = self.turn_seq;
            self.transcript
                .update(cx, |transcript, cx| transcript.append_replay(id, turn, cx));
        }
        cx.notify();
    }

    pub(in crate::agent_pane) fn start_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        if let SessionItem::UserMessage { text } = &item {
            if let Some(text) = text
                && self
                    .queued_user_messages
                    .front()
                    .is_some_and(|queued| queued == text)
            {
                self.queued_user_messages.pop_front();
                self.push_item(item, cx);
            }
            return;
        }

        if !hidden(&item) {
            self.note_visible_agent_output();
        }

        if matches!(item, SessionItem::AgentMessage { .. }) {
            self.publish_queued_user_messages(cx);
        }

        self.push_item(item, cx);
    }

    pub(in crate::agent_pane::session) fn publish_queued_user_messages(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        while let Some(text) = self.queued_user_messages.pop_front() {
            self.push_item(SessionItem::UserMessage { text: Some(text) }, cx);
        }
    }

    pub(in crate::agent_pane) fn complete_item(
        &mut self,
        item: SessionItem,
        cx: &mut Context<Self>,
    ) {
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

    pub(in crate::agent_pane) fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        select: fn(&mut SessionItem) -> Option<&mut Option<String>>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.transcript.update(cx, |transcript, _| {
            transcript.append_delta(item_id, delta, select)
        })
    }
}
