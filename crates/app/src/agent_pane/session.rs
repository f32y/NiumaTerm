use crate::agent_pane::composer::{CommandFeedbackKind, rewind_blocks_submission};
use crate::agent_pane::profile::{ANTHROPIC_MODEL_ENV, launch_env_value};
use crate::agent_pane::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Starting,
    Idle,
    Running,
    Exited,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryIdentity {
    NewConversation,
    ClaudeSession(String),
    CodexThread(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoverySnapshot {
    pub(crate) installation: InstallationKey,
    pub(crate) identity: RecoveryIdentity,
    pub(crate) profile_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryReadiness {
    Ready(RecoverySnapshot),
    Busy(String),
    MissingIdentity(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RestorationReadiness {
    Pending,
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UpdateSuspension {
    Waiting,
    Stopping,
    Updating,
    Reconnecting,
    Failed(String),
}

/// The pane's protocol session, one variant per agent kind. Both backends
/// share the [`nmt_agent_utils::chat`] event vocabulary and method surface,
/// so the pane dispatches here and stays protocol-agnostic.
pub(super) enum Backend {
    Codex(app_server::Session),
    Claude(stream_json::Session),
}

impl Backend {
    pub(super) fn process(&mut self, message: Value) -> Vec<SessionEvent> {
        match self {
            Backend::Codex(session) => session.process(message),
            Backend::Claude(session) => session.process(message),
        }
    }

    pub(super) fn send_user_message(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
    ) -> SendOutcome {
        match self {
            Backend::Codex(session) => session.send_user_message_with_skill(text, settings, skill),
            Backend::Claude(session) => session.send_user_message(text, settings),
        }
    }

    pub(super) fn adapter_commands(&self) -> Vec<SlashCommandInfo> {
        match self {
            Backend::Codex(_) => app_server::Session::adapter_commands(),
            Backend::Claude(_) => stream_json::Session::adapter_commands(),
        }
    }

    pub(super) fn execute_slash_command(
        &mut self,
        name: &str,
        arguments: &str,
    ) -> SlashCommandOutcome {
        match self {
            Backend::Codex(session) => session.execute_slash_command(name, arguments),
            Backend::Claude(session) => session.execute_slash_command(name, arguments),
        }
    }

    pub(super) fn rewind_files(&mut self, user_message_id: &str) -> SlashCommandOutcome {
        match self {
            Backend::Claude(session) => session.rewind_files(user_message_id),
            Backend::Codex(_) => SlashCommandOutcome::Rejected {
                message: "File rewind is available only for Claude.".to_string(),
            },
        }
    }

    pub(super) fn session_id(&self) -> Option<&str> {
        match self {
            Backend::Claude(session) => session.session_id(),
            Backend::Codex(_) => None,
        }
    }

    pub(super) fn recovery_identity(&self) -> Option<RecoveryIdentity> {
        match self {
            Backend::Claude(session) => session
                .session_id()
                .map(|id| RecoveryIdentity::ClaudeSession(id.to_string())),
            Backend::Codex(session) => session
                .thread_id()
                .map(|id| RecoveryIdentity::CodexThread(id.to_string())),
        }
    }

    pub(super) fn has_active_operation(&self) -> bool {
        match self {
            Backend::Claude(session) => session.has_active_operation(),
            Backend::Codex(session) => session.has_active_operation(),
        }
    }

    pub(super) fn shutdown(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        match self {
            Backend::Claude(session) => session.shutdown(timeout, force),
            Backend::Codex(session) => session.shutdown(timeout, force),
        }
    }

    pub(super) fn process_exit(&mut self) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => session.process_exit(),
            Backend::Codex(_) => Vec::new(),
        }
    }

    pub(super) fn interrupt(&mut self) {
        match self {
            Backend::Codex(session) => session.interrupt(),
            Backend::Claude(session) => session.interrupt(),
        }
    }

    pub(super) fn respond_approval(&mut self, decision: &str) {
        match self {
            Backend::Codex(session) => session.respond_approval(decision),
            Backend::Claude(session) => session.respond_approval(decision),
        }
    }
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
                reconcile_skill_binding(&text, &mut this.skill_binding);
                this.palette_selected = 0;
                this.palette_dismissed = false;
                if !matches!(
                    this.command_feedback
                        .as_ref()
                        .map(|feedback| &feedback.kind),
                    Some(CommandFeedbackKind::Queued)
                ) {
                    this.command_feedback = None;
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
            history: Vec::new(),
            history_pending: None,
            recent_sessions_mode: RecentSessionsMode::Automatic,
            recent_session_selected: 0,
            pending_resume_replay: None,
            history_scroll: VirtualListScrollHandle::new(),
            pending_approval: None,
            settings: ThreadSettings::default(),
            seed_thread_defaults: true,
            restore_thread_settings_on_ready: None,
            models: Vec::new(),
            expanded_groups: HashSet::new(),
            expanded_turns: HashSet::new(),
            completed_turn_seconds: HashMap::new(),
            expanded_rows: HashSet::new(),
            virtual_transcripts: HashMap::new(),
            turn_seq: 0,
            working_started: None,
            provider_commands: Vec::new(),
            provider_commands_ready: kind == AgentKind::Codex,
            skill_catalog: None,
            skill_binding: None,
            palette_selected: 0,
            palette_dismissed: false,
            palette_scroll: ScrollHandle::new(),
            command_feedback: None,
            queued_user_messages: VecDeque::new(),
            command_queue: VecDeque::new(),
            awaiting_command_turn: false,
            rewind_state: None,
            rewind_operation_seq: 0,
            rewind_file_completion: None,
            git_branch: None,
            git_branch_ready: false,
            git_branch_refreshing: false,
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
                        this.history_pending = Some(count);
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
                    this.history = sessions;
                    this.history_pending = None;
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
        if self.git_branch_refreshing {
            return;
        }

        let Some(cwd) = self.cwd.clone().or_else(|| {
            env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        }) else {
            self.git_branch = None;
            self.git_branch_ready = true;
            return;
        };

        self.git_branch_refreshing = true;

        let fetch = cx
            .background_executor()
            .spawn(async move { current_branch(&cwd) });

        cx.spawn(async move |this, cx| {
            let branch = fetch.await;

            this.update(cx, |this, cx| {
                this.git_branch = branch;
                this.git_branch_ready = true;
                this.git_branch_refreshing = false;
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
        self.restore_thread_settings_on_ready =
            preserve_thread_settings.then(|| self.settings.clone());

        // Replacing a conversation must clear any running or unread state
        // associated with the previous backend before the new epoch can emit.
        cx.emit(AgentPaneEvent::Interrupted);
        self.session_epoch = next_session_epoch(self.session_epoch);
        self.skill_catalog = None;
        self.skill_binding = None;
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
                        this.awaiting_command_turn = false;
                        if !this.command_queue.is_empty() {
                            this.command_queue.clear();
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
                self.awaiting_command_turn = false;
                self.command_queue.clear();
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

    pub(crate) fn installation_key(&self) -> InstallationKey {
        let provider = match self.kind {
            AgentKind::Claude => ProviderKind::Claude,
            AgentKind::Codex => ProviderKind::Codex,
        };
        let launch = agent_launch(&self.profile);
        let launcher = AgentCli::from_launch(&launch, provider.default_executable());
        InstallationKey::derive(provider, &launcher).key
    }

    /// Assess both quiescence and recoverability before any related backend
    /// is stopped. A blank tab needs no provider identity because restarting
    /// it as another blank conversation loses no conversation state.
    pub(crate) fn recovery_readiness(&self) -> RecoveryReadiness {
        if self
            .update_suspension
            .as_ref()
            .is_some_and(|state| !matches!(state, UpdateSuspension::Waiting))
        {
            return RecoveryReadiness::Busy(format!(
                "{} is already participating in an update",
                self.profile.name
            ));
        }
        if matches!(self.status, Status::Starting | Status::Running)
            || self.pending_approval.is_some()
            || self.awaiting_command_turn
            || !self.command_queue.is_empty()
            || !self.queued_user_messages.is_empty()
            || self.rewind_state.is_some()
            || self.compacting
            || self
                .session
                .as_ref()
                .is_some_and(Backend::has_active_operation)
        {
            return RecoveryReadiness::Busy(format!(
                "{} still has active agent work",
                self.profile.name
            ));
        }

        self.recovery_identity_snapshot()
    }

    pub(crate) fn recovery_identity_snapshot(&self) -> RecoveryReadiness {
        let identity = if self.items.is_empty() {
            RecoveryIdentity::NewConversation
        } else if let Some(identity) = self.session.as_ref().and_then(Backend::recovery_identity) {
            identity
        } else {
            return RecoveryReadiness::MissingIdentity(format!(
                "{} has conversation content but has not published a resumable {} identity yet",
                self.profile.name,
                self.kind.display()
            ));
        };

        RecoveryReadiness::Ready(RecoverySnapshot {
            installation: self.installation_key(),
            identity,
            profile_name: self.profile.name.clone(),
        })
    }

    pub(crate) fn prepare_update_wait(&mut self, cx: &mut Context<Self>) {
        self.update_suspension = Some(UpdateSuspension::Waiting);
        cx.notify();
    }

    pub(crate) fn cancel_update_wait(&mut self, cx: &mut Context<Self>) {
        if matches!(self.update_suspension, Some(UpdateSuspension::Waiting)) {
            self.update_suspension = None;
            cx.notify();
        }
    }

    pub(crate) fn stop_active_work_for_update(&mut self, cx: &mut Context<Self>) {
        if self.pending_approval.is_some() {
            self.respond_approval("cancel", cx);
        } else {
            self.interrupt(cx);
        }
        self.command_queue.clear();
        self.awaiting_command_turn = false;
        self.publish_queued_user_messages(cx);
        if self
            .rewind_state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.rewind_state = None;
        }
        self.compacting = false;
        self.update_suspension = Some(UpdateSuspension::Waiting);
        cx.notify();
    }

    /// Detach the backend before shutdown so its EOF cannot be mistaken for
    /// an unexpected pane exit. The transcript, draft, selection, scroll, and
    /// thread controls remain owned by this entity throughout the operation.
    pub(crate) fn suspend_for_update(
        &mut self,
        force: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.session_epoch = next_session_epoch(self.session_epoch);
        self.update_suspension = Some(UpdateSuspension::Stopping);
        self.status = Status::Starting;
        cx.emit(AgentPaneEvent::Interrupted);
        cx.notify();

        let Some(mut backend) = self.session.take() else {
            return Task::ready(Ok(()));
        };
        let worker = cx.background_executor().spawn(async move {
            let result = backend.shutdown(Duration::from_secs(5), force);
            (backend, result)
        });
        cx.spawn(async move |this, cx| {
            let (backend, result) = worker.await;
            if result.is_err() {
                let _ = this.update(cx, |this, cx| {
                    this.session = Some(backend);
                    this.update_suspension = None;
                    this.status = Status::Idle;
                    cx.notify();
                });
            }
            result
        })
    }

    pub(crate) fn mark_provider_updating(&mut self, cx: &mut Context<Self>) {
        self.update_suspension = Some(UpdateSuspension::Updating);
        cx.notify();
    }

    pub(crate) fn restore_after_update(
        &mut self,
        snapshot: &RecoverySnapshot,
        cx: &mut Context<Self>,
    ) -> bool {
        self.update_suspension = Some(UpdateSuspension::Reconnecting);
        self.last_recovery_snapshot = Some(snapshot.clone());
        let recovery = match &snapshot.identity {
            RecoveryIdentity::NewConversation => None,
            identity => Some(identity.clone()),
        };
        let started = self.start_session_with_options(recovery, true, cx);
        if !started {
            self.update_suspension = Some(UpdateSuspension::Failed(
                "The provider could not restart. Retry or start a new session.".to_string(),
            ));
        }
        cx.notify();
        started
    }

    pub(super) fn retry_update_recovery(&mut self, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.last_recovery_snapshot.clone() {
            self.restore_after_update(&snapshot, cx);
        }
    }

    pub(crate) fn restoration_readiness(&self) -> RestorationReadiness {
        match self.update_suspension.as_ref() {
            None if self.status == Status::Idle => RestorationReadiness::Ready,
            Some(UpdateSuspension::Failed(message)) => {
                RestorationReadiness::Failed(message.clone())
            }
            _ => RestorationReadiness::Pending,
        }
    }

    pub(crate) fn fail_update_recovery(&mut self, message: String, cx: &mut Context<Self>) {
        self.update_suspension = Some(UpdateSuspension::Failed(message));
        cx.notify();
    }

    pub(super) fn start_new_after_update_failure(&mut self, cx: &mut Context<Self>) {
        self.update_suspension = Some(UpdateSuspension::Reconnecting);
        if !self.start_session_with_options(None, true, cx) {
            self.update_suspension = Some(UpdateSuspension::Failed(
                "The provider could not start a new session.".to_string(),
            ));
        }
        cx.notify();
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
        self.send_text_with_skill(text, None, cx)
    }

    pub(super) fn send_text_with_skill(
        &mut self,
        text: String,
        skill: Option<&SkillReference>,
        cx: &mut Context<Self>,
    ) -> bool {
        if rewind_blocks_submission(self.rewind_state.as_ref()) {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                "Finish or cancel the current rewind before sending a message.".to_string(),
                cx,
            );
            return false;
        }
        if self.awaiting_command_turn {
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
        self.recent_sessions_mode = RecentSessionsMode::Hidden;

        match outcome {
            SendOutcome::StartedTurn => {
                self.turn_seq += 1;
                self.push(SessionItem::UserMessage { text: Some(text) }, cx);
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
        self.expanded_rows.clear();
        self.virtual_transcripts.clear();
        self.turn_seq = 0;
        self.working_started = None;
        self.compacting = false;
        self.context_window_usage = None;
        self.queued_user_messages.clear();
        self.rewind_state = None;
        self.rewind_file_completion = None;
        self.pending_resume_replay = None;
    }

    pub(super) fn reset_conversation(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        // A fresh conversation always follows the live tail again, even if
        // the previous transcript was scrolled up when it was discarded.
        self.clear_conversation_presentation();
        self.settings = ThreadSettings::default();
        self.models.clear();
        self.skill_catalog = None;
        self.skill_binding = None;
        reset_command_runtime(
            self.kind == AgentKind::Codex,
            &mut self.pending_approval,
            &mut self.provider_commands,
            &mut self.provider_commands_ready,
            &mut self.command_queue,
            &mut self.awaiting_command_turn,
            &mut self.palette_selected,
            &mut self.palette_dismissed,
        );
        self.command_feedback = None;
        self.recent_sessions_mode = RecentSessionsMode::Hidden;

        // History records belong to the provider and remain intact; only the
        // live backend and this tab's conversation presentation are reset.
        self.start_session(None, cx);
    }

    /// Start the turn clock and drive the once-a-second repaint of the live
    /// progress row; the ticker stops itself once `finish_working` clears it.
    pub(super) fn start_working(&mut self, cx: &mut Context<Self>) {
        self.working_started = Some(Instant::now());
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

    /// Settle the current turn's duration for its fold header. Duration is UI
    /// state rather than provider transcript content, so it stays outside the
    /// shared item stream.
    pub(super) fn finish_working(&mut self, cx: &mut Context<Self>) {
        if let Some(started) = self.working_started.take() {
            self.completed_turn_seconds
                .insert(self.turn_seq, started.elapsed().as_secs());
            cx.notify();
        }
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

    /// Apply one typed session event to the transcript and status line.
    pub(super) fn apply_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Ready(settings) => {
                if self.recent_sessions_mode == RecentSessionsMode::Loading
                    && let Some(replay) = self.pending_resume_replay.take()
                {
                    self.clear_conversation_presentation();
                    self.recent_sessions_mode = RecentSessionsMode::Hidden;
                    self.command_feedback = None;
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
                self.provider_commands = commands;
                self.provider_commands_ready = true;
                self.palette_selected = 0;
                cx.notify();
            }
            SessionEvent::Skills(catalog) => {
                self.skill_catalog = Some(catalog);
                self.palette_selected = 0;
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
                    if self.awaiting_command_turn && self.status != Status::Running {
                        self.awaiting_command_turn = false;
                        self.run_next_queued_command(cx);
                    }
                }
                SlashCommandOutcome::Rejected { message } => {
                    self.awaiting_command_turn = false;
                    self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                    self.run_next_queued_command(cx);
                }
                SlashCommandOutcome::NotReady => {
                    self.awaiting_command_turn = false;
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        format!("{} is not ready.", self.kind.display()),
                        cx,
                    );
                    self.run_next_queued_command(cx);
                }
            },
            SessionEvent::TurnStarted => {
                if claim_command_turn_start(&mut self.awaiting_command_turn) {
                    self.turn_seq += 1;
                    self.start_working(cx);
                }
                self.status = Status::Running;
                self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
                cx.notify();
            }
            SessionEvent::TurnCompleted { error } => {
                let completion_body = error
                    .clone()
                    .or_else(|| self.latest_agent_message().map(str::to_owned))
                    .unwrap_or_else(|| format!("{} completed the turn", self.kind.display()));
                self.awaiting_command_turn = false;
                // Compaction lives inside a turn; a flag surviving the turn
                // would leave the indicator spinning with nothing behind it.
                self.compacting = false;
                self.publish_queued_user_messages(cx);
                self.finish_working(cx);
                self.refresh_git_branch(cx);
                if self.status == Status::Running {
                    self.status = Status::Idle;
                }
                if let Some(text) = error {
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
            SessionEvent::ContextWindowUpdated(usage) => {
                self.context_window_usage = Some(usage);
                cx.notify();
            }
            SessionEvent::CompactionStarted => {
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
                self.append_delta(&item_id, &delta, |item| match item {
                    SessionItem::AgentMessage { text, .. } => Some(text),
                    _ => None,
                });
                cx.notify();
            }
            SessionEvent::ReasoningSummaryDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, |item| match item {
                    SessionItem::Reasoning { summary, .. } => Some(summary),
                    _ => None,
                });
                cx.notify();
            }
            SessionEvent::CommandOutputDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, |item| match item {
                    SessionItem::CommandExecution {
                        aggregated_output, ..
                    } => Some(aggregated_output),
                    _ => None,
                });
                cx.notify();
            }
            SessionEvent::ApprovalRequested { description } => {
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
                if let Some(completion) = self.rewind_file_completion.take() {
                    let _ = completion.send(result);
                } else {
                    warn!("received a Claude file rewind result with no pending UI operation");
                }
            }
            SessionEvent::Error { message, fatal } => {
                if self.recent_sessions_mode == RecentSessionsMode::Loading {
                    self.recent_sessions_mode = RecentSessionsMode::Open;
                    self.pending_resume_replay = None;
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
                let cancelled_queue = fatal && !self.command_queue.is_empty();
                if fatal {
                    cx.emit(AgentPaneEvent::Interrupted);
                    self.status = Status::Exited;
                    self.awaiting_command_turn = false;
                    self.command_queue.clear();
                    self.publish_queued_user_messages(cx);
                } else if self.awaiting_command_turn {
                    self.awaiting_command_turn = false;
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
                        .history
                        .iter()
                        .any(|existing| existing.id == session.id)
                    {
                        self.history.push(session);
                    }
                }
                cx.notify();
            }
            SessionEvent::Replay(items) => {
                if self.recent_sessions_mode == RecentSessionsMode::Loading {
                    self.clear_conversation_presentation();
                    self.recent_sessions_mode = RecentSessionsMode::Hidden;
                    self.command_feedback = None;
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
    pub(super) fn apply_replay(&mut self, replay: Vec<SessionItem>, cx: &mut Context<Self>) {
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

    /// Resume the picked history entry without discarding the visible
    /// conversation until the target confirms it can be opened.
    pub(super) fn resume_session(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(summary) = self.history.get(index) else {
            return;
        };
        let id = summary.id.clone();

        if self.recent_sessions_mode == RecentSessionsMode::Loading {
            return;
        }

        let previous_status = self.status;
        self.recent_sessions_mode = RecentSessionsMode::Loading;
        self.recent_session_selected = index;
        self.pending_resume_replay = None;
        self.status = Status::Starting;
        // The resumed thread's own settings are authoritative; the remembered
        // per-kind defaults must not overwrite them on the next Ready.
        self.seed_thread_defaults = false;
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
                    self.recent_sessions_mode = RecentSessionsMode::Open;
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
                        if this.recent_sessions_mode != RecentSessionsMode::Loading
                            || this.recent_session_selected != selected
                        {
                            return;
                        }

                        if this.start_session(Some(id), cx) {
                            this.pending_resume_replay = Some(replay);
                        } else {
                            this.recent_sessions_mode = RecentSessionsMode::Open;
                        }
                    });
                })
                .detach();
            }
        }
    }

    pub(super) fn start_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
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

        if matches!(item, SessionItem::AgentMessage { .. }) {
            self.publish_queued_user_messages(cx);
        }

        self.push(item, cx);
    }

    fn publish_queued_user_messages(&mut self, cx: &mut Context<Self>) {
        while let Some(text) = self.queued_user_messages.pop_front() {
            self.push(SessionItem::UserMessage { text: Some(text) }, cx);
        }
    }

    pub(super) fn complete_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
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

        cx.notify();
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
        cx.default_global::<AgentThreadDefaults>()
            .0
            .insert(self.defaults_key(), self.settings.clone());
    }

    pub(super) fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        select: fn(&mut SessionItem) -> Option<&mut Option<String>>,
    ) {
        for entry in &mut self.items {
            if entry.item.id() == Some(item_id)
                && let Some(text) = select(&mut entry.item)
            {
                text.get_or_insert_default().push_str(delta);
                break;
            }
        }
    }
}
