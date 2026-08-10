mod backend;
mod events;
#[cfg(test)]
mod tests;
mod update_recovery;

use crate::agent_pane::composer::{
    CommandFeedbackKind, restored_input_after_interruption, rewind_blocks_submission,
};
use crate::agent_pane::profile::{ANTHROPIC_MODEL_ENV, launch_env_value};
pub(super) use crate::agent_pane::session::backend::Backend;
pub(crate) use crate::agent_pane::session::backend::RecoveryIdentity;
pub(super) use crate::agent_pane::session::update_recovery::UpdateSuspension;
pub(crate) use crate::agent_pane::session::update_recovery::{
    RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};
use crate::agent_pane::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Starting,
    Idle,
    Running,
    Exited,
}

impl AgentPane {
    pub(crate) fn new(
        profile: AgentProfile,
        cwd: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let kind = AgentKind::from_profile(profile.kind);
        let name = kind.display();
        // Auto-grow wraps long prompts instead of scrolling them off-screen;
        // Enter still submits (submit_on_enter), Shift+Enter inserts a
        // newline.
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(format!("Message {name} — Enter to send"))
        });

        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            // Shift+Enter emits PressEnter too, but it inserted a newline —
            // only a plain Enter sends.
            if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                this.send_user_message(window, cx);
            } else if matches!(event, InputEvent::Change) {
                let text = this.input.read(cx).text().to_string();
                reconcile_skill_binding(&text, &mut this.palette.skill_binding);
                this.palette.selected = 0;
                this.palette.dismissed = false;
                if !matches!(
                    this.palette
                        .feedback
                        .as_ref()
                        .map(|feedback| &feedback.kind),
                    Some(CommandFeedbackKind::Queued)
                ) {
                    this.palette.feedback = None;
                }
                cx.notify();
            }
        })
        .detach();

        let mut this = Self {
            focus: cx.focus_handle(),
            agent_route: agent_process().allocate_route(),
            kind,
            profile,
            cwd,
            items: Vec::new(),
            transcript_list: {
                // Bottom alignment + tail follow give chat-log behavior: pinned
                // to the newest row until the user scrolls up, re-engaging when
                // they return to the bottom. The overdraw keeps a viewport's
                // worth of offscreen rows measured so scrolling doesn't pop.
                let state = ListState::new(0, ListAlignment::Bottom, px(512.));
                state.set_follow_mode(FollowMode::Tail);
                state
            },
            row_specs: Vec::new(),
            transcript_font: Default::default(),
            transcript_width: None,
            input,
            session: None,
            session_epoch: 0,
            status: Status::Starting,
            history_ui: SessionHistoryUi::default(),
            pending_approval: None,
            settings: ThreadSettings::default(),
            seed_thread_defaults: true,
            seed_approval_reviewer: false,
            restore_thread_settings_on_ready: None,
            models: Vec::new(),
            expanded_groups: HashSet::new(),
            expanded_turns: HashSet::new(),
            completed_turn_seconds: HashMap::new(),
            completed_turn_output_tokens: HashMap::new(),
            interrupted_turns: HashSet::new(),
            expanded_rows: HashSet::new(),
            virtual_transcripts: HashMap::new(),
            turn_seq: 0,
            working_started: None,
            working_output_tokens: None,
            unanswered_prompt: None,
            palette: SlashPalette {
                provider_commands_ready: kind == AgentKind::Codex,
                ..SlashPalette::default()
            },
            queued_user_messages: VecDeque::new(),
            rewind: RewindFlow::default(),
            git_branch_poll: GitBranchPoll::default(),
            context_window_usage: None,
            compacting: false,
            update_suspension: None,
            last_recovery_snapshot: None,
        };

        this.start_session(None, cx);
        this.refresh_git_branch(cx);

        cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |_, cx| {
                    cx.global::<AppSettings>().git_status_refresh_interval
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
        })
        .detach();

        // Claude's history comes from scanning the CLI's transcript
        // directory (Codex delivers its history over the protocol instead,
        // via `Event::History`). Two passes, both off-thread: a cheap count
        // first, so the list can reserve its final height with placeholder
        // rows, then title parsing, which swaps in the real rows.
        if kind == AgentKind::Claude {
            let cwd = this.cwd.clone();

            cx.spawn(async move |this, cx| {
                let count_cwd = cwd.clone();
                let count = cx
                    .background_executor()
                    .spawn(async move { sessions::count_sessions(count_cwd.as_deref()) })
                    .await;

                let proceed = this
                    .update(cx, |this, cx| {
                        this.history_ui.pending = Some(count);
                        cx.notify();

                        count > 0
                    })
                    .unwrap_or(false);

                if !proceed {
                    return;
                }

                // Title parsing races a short hold: on a warm SSD it
                // finishes within a frame, so without the hold the skeleton
                // rows would never be visible and the swap would read as a
                // flicker.
                let load = cx
                    .background_executor()
                    .spawn(async move { sessions::list_sessions(cwd.as_deref()) });

                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;

                let sessions = load.await;

                let _ = this.update(cx, |this, cx| {
                    this.history_ui.sessions = sessions;
                    this.history_ui.pending = None;
                    cx.notify();
                });
            })
            .detach();
        }

        this
    }

    pub(crate) fn agent_route(&self) -> &AgentRoute {
        &self.agent_route
    }

    pub(super) fn refresh_git_branch(&mut self, cx: &mut Context<Self>) {
        if !self.git_branch_poll.begin_refresh() {
            return;
        }

        let Some(cwd) = self.cwd.clone().or_else(|| {
            env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        }) else {
            self.git_branch_poll.complete(None);
            return;
        };

        let fetch = cx
            .background_executor()
            .spawn(async move { current_branch(&cwd) });

        cx.spawn(async move |this, cx| {
            let branch = fetch.await;

            this.update(cx, |this, cx| {
                this.git_branch_poll.complete(branch);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn emit_lifecycle(
        &self,
        kind: AgentEventKind,
        title: &str,
        body: &str,
        cx: &mut Context<Self>,
    ) {
        cx.emit(AgentPaneEvent::Lifecycle(AgentEvent {
            route: self.agent_route.clone(),
            agent: self.kind.id().to_string(),
            session_id: format!("agent-tab-{}", self.session_epoch),
            turn_id: (kind != AgentEventKind::SessionStarted)
                .then(|| format!("turn-{}", self.turn_seq)),
            kind,
            title: normalize_title(title),
            body: normalize_body(body),
        }));
    }

    pub(super) fn latest_agent_message(&self) -> Option<&str> {
        self.items.iter().rev().find_map(|entry| match &entry.item {
            SessionItem::AgentMessage {
                text: Some(text), ..
            } if entry.turn == self.turn_seq && !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
    }

    /// Spawn the backend process (optionally resuming a persisted Claude
    /// session) and pump its messages onto the UI thread. Channel closure is
    /// the EOF signal (the sender is owned by the reader thread). Does not
    /// notify — callers decide whether a repaint is due.
    pub(super) fn start_session(&mut self, resume: Option<String>, cx: &mut Context<Self>) -> bool {
        self.start_session_with_options(resume.map(RecoveryIdentity::ClaudeSession), false, cx)
    }

    pub(super) fn start_session_with_options(
        &mut self,
        recovery: Option<RecoveryIdentity>,
        preserve_thread_settings: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        // The pane's profile is a snapshot from when the tab opened; profile
        // edits in settings don't reach into live panes. Re-resolving by
        // name at every (re)start picks them up, so a new conversation
        // launches with the profile as currently configured. A renamed or
        // deleted profile keeps the snapshot so the tab still works.
        if let Some(fresh) = cx
            .global::<AppSettings>()
            .agent_profiles
            .iter()
            .find(|p| p.kind == self.profile.kind && p.name == self.profile.name)
        {
            self.profile = fresh.clone();
        }

        // The profile model is known before either CLI completes its
        // handshake, so the picker need not flash the backend default while a
        // custom endpoint is starting.
        if !preserve_thread_settings && let Some(model) = self.profile_model() {
            self.settings.model = Some(model);
        }

        let kind = self.kind;
        let name = kind.display();
        let cwd = self.cwd.clone();

        self.seed_thread_defaults = !preserve_thread_settings && recovery.is_none();
        self.seed_approval_reviewer =
            !preserve_thread_settings && recovery.is_some() && kind == AgentKind::Codex;
        self.restore_thread_settings_on_ready =
            preserve_thread_settings.then(|| self.settings.clone());

        // Replacing a conversation must clear any running or unread state
        // associated with the previous backend before the new epoch can emit.
        cx.emit(AgentPaneEvent::Interrupted);
        self.session_epoch = next_session_epoch(self.session_epoch);
        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;
        let epoch = self.session_epoch;

        let (tx, mut rx) = mpsc::unbounded::<Value>();
        let deliver = move |message| {
            let _ = tx.unbounded_send(message);
        };
        let launch = agent_launch(&self.profile);
        // Env names only: the values can carry API keys.
        info!(
            "agent session start: profile=\"{}\", executable=\"{}\", env=[{}]",
            self.profile.name,
            launch.executable,
            launch
                .env
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let spawned = match (kind, recovery) {
            (AgentKind::Codex, Some(RecoveryIdentity::CodexThread(thread_id))) => {
                app_server::Session::spawn_resuming(
                    &launch,
                    cwd,
                    thread_id,
                    true,
                    deliver,
                    |line| warn!("codex app-server: {line}"),
                )
                .map(Backend::Codex)
            }
            (AgentKind::Codex, _) => app_server::Session::spawn(&launch, cwd, deliver, |line| {
                warn!("codex app-server: {line}")
            })
            .map(Backend::Codex),
            (AgentKind::Claude, recovery) => {
                let session_id = match recovery {
                    Some(RecoveryIdentity::ClaudeSession(id)) => Some(id),
                    _ => None,
                };
                stream_json::Session::spawn(&launch, cwd, session_id, deliver, |line| {
                    warn!("claude: {line}")
                })
                .map(Backend::Claude)
            }
        };

        match spawned {
            Ok(session) => {
                self.session = Some(session);
                self.status = Status::Starting;

                cx.spawn(async move |this, cx| {
                    while let Some(message) = rx.next().await {
                        let updated = this.update(cx, |this, cx| {
                            // A newer session owns the pane now; this pump's
                            // messages belong to the replaced process.
                            if !is_current_session_epoch(this.session_epoch, epoch) {
                                return false;
                            }

                            let events = match this.session.as_mut() {
                                Some(session) => session.process(message),
                                None => Vec::new(),
                            };

                            for event in events {
                                this.apply_event(event, cx);
                            }

                            true
                        });

                        if !updated.unwrap_or(false) {
                            return;
                        }
                    }

                    let _ = this.update(cx, |this, cx| {
                        // A deliberately replaced session exits by design;
                        // only the live session's death is worth a line.
                        if !is_current_session_epoch(this.session_epoch, epoch) {
                            return;
                        }
                        let exit_events = this
                            .session
                            .as_mut()
                            .map(Backend::process_exit)
                            .unwrap_or_default();
                        for event in exit_events {
                            this.apply_event(event, cx);
                        }
                        cx.emit(AgentPaneEvent::Interrupted);
                        this.status = Status::Exited;
                        if matches!(this.update_suspension, Some(UpdateSuspension::Reconnecting)) {
                            this.update_suspension = Some(UpdateSuspension::Failed(format!(
                                "{name} exited before the conversation was restored."
                            )));
                        }
                        this.palette.awaiting_command_turn = false;
                        if !this.palette.command_queue.is_empty() {
                            this.palette.command_queue.clear();
                            this.set_command_feedback(
                                CommandFeedbackKind::Error,
                                format!("Queued commands were cancelled because {name} exited."),
                                cx,
                            );
                        }
                        this.publish_queued_user_messages(cx);
                        this.finish_working(cx);
                        this.push(
                            SessionItem::Error {
                                text: format!("{name} exited."),
                            },
                            cx,
                        );
                    });
                })
                .detach();
                true
            }
            Err(err) => {
                cx.emit(AgentPaneEvent::Interrupted);
                self.status = Status::Exited;
                self.palette.awaiting_command_turn = false;
                self.palette.command_queue.clear();
                self.queued_user_messages.clear();
                self.items.push(Entry {
                    at: Local::now().format("%H:%M").to_string(),
                    turn: self.turn_seq,
                    item: SessionItem::Error {
                        text: format!("Failed to start {name}: {err}"),
                    },
                });
                false
            }
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(crate) fn kind(&self) -> AgentKind {
        self.kind
    }

    /// Name of the launch profile, persisted with the tab snapshot so
    /// restore reopens the same profile.
    pub(crate) fn profile_name(&self) -> &str {
        &self.profile.name
    }

    /// Send one user message through the session with full turn bookkeeping;
    /// also used for UI-generated messages such as the `/effort` command.
    /// Returns false when the session isn't ready yet.
    pub(super) fn send_text(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        self.send_text_inner(text, None, false, cx)
    }

    pub(super) fn send_text_with_skill(
        &mut self,
        text: String,
        skill: Option<&SkillReference>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.send_text_inner(text, skill, true, cx)
    }

    fn send_text_inner(
        &mut self,
        text: String,
        skill: Option<&SkillReference>,
        restore_on_interrupt: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if rewind_blocks_submission(self.rewind.state.as_ref()) {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                "Finish or cancel the current rewind before sending a message.".to_string(),
                cx,
            );
            return false;
        }
        if self.palette.awaiting_command_turn {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                "A command is starting; wait for its turn to begin.".to_string(),
                cx,
            );
            return false;
        }

        let settings = self.settings.clone();
        let outcome = match self.session.as_mut() {
            Some(session) => session.send_user_message(&text, &settings, skill),
            None => SendOutcome::NotReady,
        };

        if outcome == SendOutcome::NotReady {
            self.push(
                SessionItem::Error {
                    text: format!(
                        "{} is still starting; try again in a moment.",
                        self.kind.display()
                    ),
                },
                cx,
            );
            return false;
        }

        // The first message commits this tab to its conversation; the
        // history list is no longer offered.
        self.history_ui.mode = RecentSessionsMode::Hidden;

        match outcome {
            SendOutcome::StartedTurn => {
                self.turn_seq += 1;
                let unanswered_prompt = restore_on_interrupt.then(|| UnansweredPrompt {
                    turn: self.turn_seq,
                    text: text.clone(),
                    skill: skill.cloned(),
                });
                self.push(SessionItem::UserMessage { text: Some(text) }, cx);
                self.unanswered_prompt = unanswered_prompt;
                self.start_working(cx);
            }
            SendOutcome::Steered => {
                self.queued_user_messages.push_back(text);
                cx.notify();
            }
            SendOutcome::NotReady => unreachable!(),
        }

        true
    }

    pub(super) fn clear_conversation_presentation(&mut self) {
        self.items.clear();
        self.scroll_transcript_to_bottom();
        self.expanded_groups.clear();
        self.expanded_turns.clear();
        self.completed_turn_seconds.clear();
        self.completed_turn_output_tokens.clear();
        self.interrupted_turns.clear();
        self.expanded_rows.clear();
        self.virtual_transcripts.clear();
        self.turn_seq = 0;
        self.working_started = None;
        self.working_output_tokens = None;
        self.unanswered_prompt = None;
        self.compacting = false;
        self.context_window_usage = None;
        self.queued_user_messages.clear();
        self.rewind.state = None;
        self.rewind.file_completion = None;
        self.history_ui.pending_resume_replay = None;
    }

    pub(super) fn reset_conversation(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        // A fresh conversation always follows the live tail again, even if
        // the previous transcript was scrolled up when it was discarded.
        self.clear_conversation_presentation();
        self.settings = ThreadSettings::default();
        self.models.clear();
        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;
        reset_command_runtime(
            self.kind == AgentKind::Codex,
            &mut self.pending_approval,
            &mut self.palette.provider_commands,
            &mut self.palette.provider_commands_ready,
            &mut self.palette.command_queue,
            &mut self.palette.awaiting_command_turn,
            &mut self.palette.selected,
            &mut self.palette.dismissed,
        );
        self.palette.feedback = None;
        self.history_ui.mode = RecentSessionsMode::Hidden;

        // History records belong to the provider and remain intact; only the
        // live backend and this tab's conversation presentation are reset.
        self.start_session(None, cx);
    }

    /// Start the turn clock and drive the once-a-second repaint of the live
    /// progress row; the ticker stops itself once `finish_working` clears it.
    pub(super) fn start_working(&mut self, cx: &mut Context<Self>) {
        self.working_started = Some(Instant::now());
        self.working_output_tokens = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let ticking = this.update(cx, |this, cx| {
                    if this.working_started.is_some() {
                        cx.notify();
                        true
                    } else {
                        false
                    }
                });

                if !ticking.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Settle the current turn's duration and exact output usage for its status
    /// row. These values are UI state rather than provider transcript content,
    /// so they stay outside the shared item stream.
    pub(super) fn finish_working(&mut self, cx: &mut Context<Self>) {
        let output_tokens = self.working_output_tokens.take();
        if let Some(started) = self.working_started.take() {
            if !self.interrupted_turns.contains(&self.turn_seq) {
                self.completed_turn_seconds
                    .insert(self.turn_seq, started.elapsed().as_secs());
            }
            if let Some(output_tokens) = output_tokens {
                self.completed_turn_output_tokens
                    .insert(self.turn_seq, output_tokens);
            }
            cx.notify();
        }
    }

    fn note_visible_agent_output(&mut self) {
        if self
            .unanswered_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.turn == self.turn_seq)
        {
            self.unanswered_prompt = None;
        }
    }

    pub(super) fn interrupt_from_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let interrupted_turn = self.working_started.is_some().then_some(self.turn_seq);
        if let Some(prompt) = self
            .unanswered_prompt
            .take()
            .filter(|prompt| prompt.turn == self.turn_seq && self.working_started.is_some())
        {
            self.working_started = None;
            self.working_output_tokens = None;
            self.completed_turn_seconds.remove(&prompt.turn);
            self.completed_turn_output_tokens.remove(&prompt.turn);
            self.compacting = false;

            let current = self.input.read(cx).text().to_string();
            let restored = restored_input_after_interruption(&prompt.text, &current);
            let cursor = restored.len();
            self.input.update(cx, |input, cx| {
                input.set_value(restored, window, cx);
                input.set_selected_range(cursor..cursor, cx);
            });
            self.palette.skill_binding = prompt.skill;
        }
        if let Some(turn) = interrupted_turn {
            self.interrupted_turns.insert(turn);
        }

        self.interrupt(cx);
    }

    pub(super) fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_mut() {
            session.interrupt();
            cx.emit(AgentPaneEvent::Interrupted);
            cx.notify();
        }
    }

    pub(super) fn respond_approval(&mut self, decision: &str, cx: &mut Context<Self>) {
        // The card is dismissed immediately for a snappy UI; the session's
        // `ApprovalResolved` confirmation is then an idempotent status refresh.
        self.pending_approval = None;
        self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

        if let Some(session) = self.session.as_mut() {
            session.respond_approval(decision);
        }
        cx.notify();
    }

    /// Resume the picked history entry without discarding the visible
    /// conversation until the target confirms it can be opened.
    pub(super) fn resume_session(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(summary) = self.history_ui.sessions.get(index) else {
            return;
        };
        let id = summary.id.clone();

        if self.history_ui.mode == RecentSessionsMode::Loading {
            return;
        }

        let previous_status = self.status;
        self.history_ui.mode = RecentSessionsMode::Loading;
        self.history_ui.selected = index;
        self.history_ui.pending_resume_replay = None;
        self.status = Status::Starting;
        // Keep the resumed thread's stored controls except for the reviewer,
        // which is a local preference shared across this Codex profile.
        self.seed_thread_defaults = false;
        self.seed_approval_reviewer = self.kind == AgentKind::Codex;
        self.set_command_feedback(
            CommandFeedbackKind::Notice,
            "Opening recent session…".to_string(),
            cx,
        );

        match self.kind {
            AgentKind::Codex => {
                if let Some(Backend::Codex(session)) = self.session.as_mut() {
                    session.resume_thread(&id);
                } else {
                    self.history_ui.mode = RecentSessionsMode::Open;
                    self.status = previous_status;
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        "Codex is not ready to open a recent session.".to_string(),
                        cx,
                    );
                }
            }
            AgentKind::Claude => {
                let cwd = self.cwd.clone();
                let replay_id = id.clone();
                let selected = index;

                cx.spawn(async move |this, cx| {
                    let replay = cx
                        .background_executor()
                        .spawn(async move { sessions::load_replay(cwd.as_deref(), &replay_id) })
                        .await;

                    let _ = this.update(cx, |this, cx| {
                        if this.history_ui.mode != RecentSessionsMode::Loading
                            || this.history_ui.selected != selected
                        {
                            return;
                        }

                        if this.start_session(Some(id), cx) {
                            this.history_ui.pending_resume_replay = Some(replay);
                        } else {
                            this.history_ui.mode = RecentSessionsMode::Open;
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// Key for the per-profile thread-settings memory; a profile without a
    /// name shares the agent-kind bucket (also the key older local_state
    /// snapshots used).
    pub(super) fn defaults_key(&self) -> String {
        if self.profile.name.trim().is_empty() {
            self.kind.id().to_string()
        } else {
            self.profile.name.clone()
        }
    }

    /// Effective startup model after protocol mapping and user environment
    /// overrides. Claude resolves `ANTHROPIC_MODEL` with last-value-wins
    /// semantics; Codex receives the profile field over app-server RPC.
    pub(super) fn profile_model(&self) -> Option<String> {
        let launch = agent_launch(&self.profile);
        match self.kind {
            AgentKind::Claude => launch_env_value(&launch, ANTHROPIC_MODEL_ENV),
            AgentKind::Codex => launch.model,
        }
    }

    /// Remember the pane's current thread settings as the defaults for future
    /// conversations launched from this profile. Called after every
    /// user-driven settings change (dropdowns and slash commands).
    pub(super) fn remember_thread_defaults(&self, cx: &mut Context<Self>) {
        let stored = {
            let defaults = cx.default_global::<AgentThreadDefaults>();
            defaults
                .0
                .insert(self.defaults_key(), self.settings.clone());
            defaults.to_local_state()
        };

        if let Err(err) = local_state::save_agent_defaults(&stored) {
            warn!("failed to save agent defaults to local_state.toml: {err}");
        }
    }
}
