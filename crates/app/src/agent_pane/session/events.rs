use crate::agent_pane::composer::CommandFeedbackKind;
use crate::agent_pane::session::{Status, UpdateSuspension};
use crate::agent_pane::transcript::hidden;
use crate::agent_pane::*;

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
                    self.clear_conversation_presentation();
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
                let effort = settings.effort.clone().or(self.settings.effort.take());
                let mut next = ThreadSettings { effort, ..settings };

                // Fresh conversations seed from the remembered per-profile
                // picks (a remembered value wins over the CLI default);
                // resumed threads and later Ready confirmations take the
                // backend settings as-is. Older local_state entries were
                // keyed by agent
                // kind, so that key still works as a fallback.
                let seed_thread_defaults = take(&mut self.seed_thread_defaults);
                if seed_thread_defaults
                    && let Some(stored) =
                        cx.try_global::<AgentThreadDefaults>().and_then(|defaults| {
                            defaults
                                .0
                                .get(&self.defaults_key())
                                .or_else(|| defaults.0.get(self.kind.id()))
                        })
                {
                    next = ThreadSettings {
                        model: stored.model.clone().or(next.model),
                        approval: stored.approval.clone().or(next.approval),
                        sandbox: stored.sandbox.clone().or(next.sandbox),
                        effort: stored.effort.clone().or(next.effort),
                        tier: stored.tier.clone().or(next.tier),
                    };
                }

                // A profile model is the startup default and therefore beats
                // remembered per-profile picker state for a fresh thread.
                // Later Ready events report live model changes and must pass
                // through unchanged.
                if seed_thread_defaults && let Some(model) = self.profile_model() {
                    next.model = Some(model);
                }

                if let Some(restored) = self.restore_thread_settings_on_ready.take() {
                    next = ThreadSettings {
                        model: restored.model.or(next.model),
                        approval: restored.approval.or(next.approval),
                        sandbox: restored.sandbox.or(next.sandbox),
                        effort: restored.effort.or(next.effort),
                        tier: restored.tier.or(next.tier),
                    };
                }

                self.settings = next;
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
                cx.notify();
            }
            SessionEvent::Models(models) => {
                self.models = models;
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
                        format!("/{name} accepted."),
                        cx,
                    );
                }
                SlashCommandOutcome::Completed { message } => {
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        message.unwrap_or_else(|| format!("/{name} completed.")),
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
                        format!("{} is not ready.", self.kind.display()),
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
                let interrupted_by_user = self.interrupted_turns.contains(&self.turn_seq);
                let completion_body = error
                    .clone()
                    .or_else(|| self.latest_agent_message().map(str::to_owned))
                    .unwrap_or_else(|| format!("{} completed the turn", self.kind.display()));
                self.palette.awaiting_command_turn = false;
                self.unanswered_prompt = None;
                // Compaction lives inside a turn; a flag surviving the turn
                // would leave the indicator spinning with nothing behind it.
                self.compacting = false;
                self.publish_queued_user_messages(cx);
                self.finish_working(cx);
                self.refresh_git_branch(cx);
                if self.status == Status::Running {
                    self.status = Status::Idle;
                }
                if let Some(text) = error
                    && !interrupted_by_user
                {
                    self.push(SessionItem::Error { text }, cx);
                }
                self.emit_lifecycle(
                    AgentEventKind::Stopped,
                    &format!("{} finished", self.kind.display()),
                    &completion_body,
                    cx,
                );
                self.run_next_queued_command(cx);
                cx.notify();
            }
            SessionEvent::TurnOutputTokensUpdated(output_tokens) => {
                if self.working_started.is_some() {
                    self.working_output_tokens = Some(output_tokens);
                    cx.notify();
                }
            }
            SessionEvent::ContextWindowUpdated(usage) => {
                self.context_window_usage = Some(usage);
                cx.notify();
            }
            SessionEvent::CompactionStarted => {
                self.note_visible_agent_output();
                self.compacting = true;
                cx.notify();
            }
            SessionEvent::CompactionFinished { error } => {
                self.compacting = false;
                // A failed compaction is not the turn's own failure, so it needs
                // its own row: the turn continues (and usually then dies on an
                // over-length prompt) with no other trace of why.
                if let Some(text) = error {
                    self.push(SessionItem::Error { text }, cx);
                }
                cx.notify();
            }
            SessionEvent::ItemStarted(item) => self.start_item(item, cx),
            SessionEvent::ItemCompleted(item) => self.complete_item(item, cx),
            SessionEvent::AgentMessageDelta { item_id, delta } => {
                let visible = self.append_delta(&item_id, &delta, |item| match item {
                    SessionItem::AgentMessage { text, .. } => Some(text),
                    _ => None,
                });
                if visible {
                    self.note_visible_agent_output();
                }
                cx.notify();
            }
            SessionEvent::ReasoningSummaryDelta { item_id, delta } => {
                let visible = self.append_delta(&item_id, &delta, |item| match item {
                    SessionItem::Reasoning { summary, .. } => Some(summary),
                    _ => None,
                });
                if visible {
                    self.note_visible_agent_output();
                }
                cx.notify();
            }
            SessionEvent::CommandOutputDelta { item_id, delta } => {
                let visible = self.append_delta(&item_id, &delta, |item| match item {
                    SessionItem::CommandExecution {
                        aggregated_output, ..
                    } => Some(aggregated_output),
                    _ => None,
                });
                if visible {
                    self.note_visible_agent_output();
                }
                cx.notify();
            }
            SessionEvent::ApprovalRequested { description } => {
                self.note_visible_agent_output();
                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &format!("{} needs input", self.kind.display()),
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
                        format!("Could not open the selected session: {message}"),
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
                self.push(SessionItem::Error { text: message }, cx);
                if cancelled_queue {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        "Queued commands were cancelled because the session failed.".to_string(),
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
            SessionEvent::Replay(items) => {
                if self.history_ui.mode == RecentSessionsMode::Loading {
                    self.clear_conversation_presentation();
                    self.history_ui.mode = RecentSessionsMode::Hidden;
                    self.palette.feedback = None;
                }
                self.apply_replay(items, cx);
            }
            // No status line in the UI anymore; the live working row and the
            // Stop button carry the running state.
            SessionEvent::StatusDetail(_) => {}
        }
    }

    /// Pre-fill the transcript with a resumed session's reconstructed
    /// conversation. Replay entries share one turn and carry no fold header,
    /// so they render as a plain chronological stream above the new turns.
    pub(in crate::agent_pane) fn apply_replay(
        &mut self,
        replay: Vec<SessionItem>,
        cx: &mut Context<Self>,
    ) {
        for item in replay {
            // Replayed entries predate this pane; they get no wall-clock
            // hover stamp.
            self.items.push(Entry {
                at: String::new(),
                turn: self.turn_seq,
                item,
            });
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
                self.push(item, cx);
            }
            return;
        }

        if !hidden(&item) {
            self.note_visible_agent_output();
        }

        if matches!(item, SessionItem::AgentMessage { .. }) {
            self.publish_queued_user_messages(cx);
        }

        self.push(item, cx);
    }

    pub(in crate::agent_pane::session) fn publish_queued_user_messages(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        while let Some(text) = self.queued_user_messages.pop_front() {
            self.push(SessionItem::UserMessage { text: Some(text) }, cx);
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

        let known = self
            .items
            .iter()
            .any(|entry| entry.item.id() == Some(id.as_str()));

        // A completed item this pane never saw start (e.g. joined mid-turn)
        // still gets a transcript entry.
        if !known {
            self.start_item(item.clone(), cx);
        }

        for entry in &mut self.items {
            if entry.item.merge_completed(&item) {
                break;
            }
        }

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
    ) -> bool {
        for entry in &mut self.items {
            if entry.item.id() == Some(item_id)
                && let Some(text) = select(&mut entry.item)
            {
                let text = text.get_or_insert_default();
                text.push_str(delta);
                return !text.trim().is_empty();
            }
        }

        false
    }
}
