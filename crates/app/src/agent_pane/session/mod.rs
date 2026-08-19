mod backend;
mod conversation;
mod events;
#[cfg(test)]
mod tests;
mod update_recovery;

use std::fs;
use std::sync::Arc;

use gpui::Image;
use nmt_i18n::i18n;

use crate::agent_pane::composer::{
    CommandFeedbackKind, PaletteControl, restored_input_after_interruption,
    rewind_blocks_submission,
};
use crate::agent_pane::profile::{ANTHROPIC_MODEL_ENV, launch_env_value};
pub(super) use crate::agent_pane::session::backend::Backend;
pub(crate) use crate::agent_pane::session::backend::RecoveryIdentity;
#[cfg(test)]
pub(in crate::agent_pane) use crate::agent_pane::session::backend::TestBackend;
pub(super) use crate::agent_pane::session::update_recovery::UpdateSuspension;
pub(crate) use crate::agent_pane::session::update_recovery::{
    RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};
use crate::agent_pane::*;

/// A pane's attachment files live only as long as the pane: the harness that
/// reads them has already read what it was sent, and nothing else refers to
/// them. A directory that was never created removes cleanly.
impl Drop for AgentPane {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(scratch_dir(self.agent_route.as_str()));
    }
}

/// How long the composer's "last response" reading stays accurate, given how
/// old it already is. Matches the coarsest unit the label shows, so the pane
/// redraws exactly as often as the words change.
fn response_age_tick(age: Duration) -> Duration {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;

    match age.as_secs() {
        ..MINUTE => Duration::from_secs(1),
        MINUTE..HOUR => Duration::from_secs(MINUTE),
        _ => Duration::from_secs(HOUR),
    }
}

/// Whether two recorded working directories name the same place. Compared
/// case-insensitively with separators normalized, because the two sides come
/// from different writers: one from the tab's own configuration, the other
/// from whatever the agent recorded when it ran.
pub(in crate::agent_pane) fn directories_match(left: Option<&str>, right: Option<&str>) -> bool {
    let normalize = |path: &str| {
        path.trim_end_matches(['/', '\\'])
            .replace('\\', "/")
            .to_lowercase()
    };

    match (left, right) {
        (Some(left), Some(right)) => normalize(left) == normalize(right),
        // A row that records no directory says nothing about belonging
        // elsewhere, so it stays resumable in place.
        _ => true,
    }
}

/// How a session's directory reads in a list: its last two components, which
/// is enough to tell projects apart without spending the row's width on a
/// path that is mostly shared prefix.
pub(in crate::agent_pane) fn directory_label(cwd: &str) -> String {
    let parts: Vec<&str> = cwd
        .trim_end_matches(['/', '\\'])
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();

    match parts.len() {
        0 => cwd.to_string(),
        1 => parts[0].to_string(),
        length => format!("{}/{}", parts[length - 2], parts[length - 1]),
    }
}

/// The filesystem history a scope covers. Only a backend that reads its own
/// transcripts takes this route; one that lists over the protocol asks its
/// server for the scope instead.
fn count_scoped_sessions(scope: SessionScope, cwd: Option<&str>) -> usize {
    match scope {
        SessionScope::CurrentDirectory => sessions::count_sessions(cwd),
        SessionScope::AllDirectories => sessions::count_all_sessions(),
    }
}

fn list_scoped_sessions(scope: SessionScope, cwd: Option<&str>) -> Vec<SessionSummary> {
    match scope {
        SessionScope::CurrentDirectory => sessions::list_sessions(cwd),
        SessionScope::AllDirectories => sessions::list_all_sessions(),
    }
}

/// Cap for a tab title taken from a prompt. The strip truncates whatever it is
/// given, so this only bounds what the tab carries around.
const TAB_TITLE_CHARS: usize = 60;

/// The name a composed prompt gives its tab: its first non-empty line. A slash
/// command names nothing — it instructs the CLI rather than stating a subject,
/// and the settings controls send some of them on the user's behalf — so a
/// conversation that opens with one waits for the message that follows.
fn tab_title_from_prompt(text: &str) -> Option<String> {
    let line = text.lines().find(|line| !line.trim().is_empty())?.trim();

    (!line.starts_with('/')).then(|| line.chars().take(TAB_TITLE_CHARS).collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Starting,
    Idle,
    Running,
    Exited,
}

/// Show a task snapshot only against the parent session it was produced for.
/// Provider adapters publish snapshots asynchronously, so a snapshot can still
/// be held when the pane has already moved to another session or has no
/// session id yet; in both cases the view must render nothing rather than
/// another conversation's children.
fn scoped_background_tasks<'a>(
    parent: Option<&BackgroundTaskKey>,
    snapshot: Option<&'a BackgroundTaskSnapshot>,
) -> Option<&'a BackgroundTaskSnapshot> {
    let parent = parent?;
    let snapshot = snapshot?;
    (&snapshot.parent_session == parent).then_some(snapshot)
}

impl AgentPane {
    pub(crate) fn new(
        profile: AgentProfile,
        cwd: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_resuming(profile, cwd, None, window, cx)
    }

    /// A pane whose first session continues `resume` instead of opening a
    /// fresh conversation. Used to reopen a listed conversation in the
    /// directory it ran in, which is a different one than the tab that
    /// listed it.
    pub(crate) fn new_resuming(
        profile: AgentProfile,
        cwd: Option<String>,
        resume: Option<RecoveryIdentity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let kind = AgentKind::from_profile(profile.kind);
        let input_history_scope = InputHistoryScope::local(kind, cwd.as_deref());
        let name = kind.display();
        // Auto-grow wraps long prompts instead of scrolling them off-screen.
        // The view intercepts modified Enter actions before this input's
        // submit-on-enter behavior runs.
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(i18n("agent-session-message-placeholder").replace("{name}", name))
        });

        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(
                event,
                InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                }
            ) {
                this.send_user_message(window, cx);
            } else if matches!(event, InputEvent::Change) {
                let text = this.input.read(cx).text().to_string();
                // The text is the record of which images the message still
                // carries, so an edit that removed a placeholder removes its
                // image here, whichever way the text was edited.
                this.sync_attachments(&text, window, cx);
                this.input_history_navigation.reset();
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

        let transcript = cx.new(|_| TranscriptView::new(kind, cwd.clone()));

        let mut this = Self {
            focus: cx.focus_handle(),
            agent_route: agent_process().allocate_route(),
            kind,
            profile,
            cwd,
            input_history_scope,
            input_history_navigation: InputHistoryNavigation::default(),
            attachments: PendingAttachments::default(),
            last_response_at: None,
            conversation_named: false,
            transcript,
            input,
            session: None,
            session_epoch: 0,
            status: Status::Starting,
            history_ui: SessionHistoryUi::default(),
            pending_approval: None,
            pending_questions: None,
            settings: ThreadSettings::default(),
            seed_thread_defaults: true,
            seed_approval_reviewer: false,
            restore_thread_settings_on_ready: None,
            models: Vec::new(),
            approval_presets: Vec::new(),
            turn_seq: 0,
            turn_submitted_at: None,
            first_output_latency: None,
            unanswered_prompt: None,
            pending_interrupt: None,
            palette: SlashPalette {
                provider_commands_ready: !kind.caps().async_command_discovery,
                ..SlashPalette::default()
            },
            queued_user_messages: VecDeque::new(),
            rewind: RewindFlow::default(),
            git_branch_poll: GitBranchPoll::default(),
            context_window_usage: None,
            context_composition: None,
            goal: None,
            plan_mode: false,
            session_stats: None,
            update_suspension: None,
            last_recovery_snapshot: None,
            restored_task_session: None,
            background_tasks: None,
            background_task_transcripts: HashMap::new(),
            workflows: WorkflowUi::default(),
        };

        this.start_session_with_options(resume, false, |_, _, _| {}, cx);
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

        this.load_filesystem_history(cx);

        this
    }

    /// Widen the session list to every directory, or narrow it back to this
    /// tab's. The rows on screen answered the previous scope, so they go; the
    /// reload republishes what the new one covers. A backend that lists over
    /// the protocol is asked again, one that reads its own transcripts is
    /// rescanned.
    pub(super) fn toggle_history_scope(&mut self, cx: &mut Context<Self>) {
        self.history_ui.scope = match self.history_ui.scope {
            SessionScope::CurrentDirectory => SessionScope::AllDirectories,
            SessionScope::AllDirectories => SessionScope::CurrentDirectory,
        };
        self.history_ui.sessions.clear();
        self.history_ui.showing_search = false;
        self.history_ui.selected = 0;

        if let Some(session) = self.session.as_mut() {
            session.request_history(self.history_ui.scope);
        }
        self.load_filesystem_history(cx);

        cx.notify();
    }

    /// History read from the CLI's transcript directory, for a harness that
    /// does not deliver it over the protocol as `Event::History`. Two passes,
    /// both off-thread: a cheap count first, so the list can reserve its final
    /// height with placeholder rows, then title parsing, which swaps in the
    /// real rows.
    fn load_filesystem_history(&mut self, cx: &mut Context<Self>) {
        if !self.kind.caps().filesystem_session_history {
            return;
        }

        let cwd = self.cwd.clone();
        let scope = self.history_ui.scope;

        cx.spawn(async move |this, cx| {
            let count_cwd = cwd.clone();
            let count = cx
                .background_executor()
                .spawn(async move { count_scoped_sessions(scope, count_cwd.as_deref()) })
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

            // Title parsing races a short hold: on a warm SSD it finishes
            // within a frame, so without the hold the skeleton rows would
            // never be visible and the swap would read as a flicker.
            let load = cx
                .background_executor()
                .spawn(async move { list_scoped_sessions(scope, cwd.as_deref()) });

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

    pub(crate) fn agent_route(&self) -> &AgentRoute {
        &self.agent_route
    }

    pub(crate) fn agent_kind(&self) -> AgentKind {
        self.kind
    }

    /// The tab's working directory, which transcript links resolve against.
    pub(crate) fn working_directory(&self) -> Option<String> {
        self.cwd.clone()
    }

    /// Append one item to the conversation, tagged with the current turn so
    /// settled turns fold as one unit.
    pub(in crate::agent_pane) fn push_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        self.push_item_with_images(item, Vec::new(), cx);
    }

    /// Append an item along with the images it carried, which only a sent
    /// user message has.
    pub(in crate::agent_pane) fn push_item_with_images(
        &mut self,
        item: SessionItem,
        images: Vec<Arc<Image>>,
        cx: &mut Context<Self>,
    ) {
        let turn = self.turn_seq;
        self.transcript
            .update(cx, |transcript, cx| transcript.push(turn, item, images, cx));
        cx.notify();
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

    pub(super) fn latest_agent_message(&self, cx: &App) -> Option<String> {
        self.transcript
            .read(cx)
            .latest_agent_message(self.turn_seq)
            .map(str::to_owned)
    }

    /// Request the backend process (optionally resuming a persisted Claude
    /// session) and pump its messages onto the UI thread. Channel closure is
    /// the EOF signal (the sender is owned by the reader thread). Returns
    /// before the process exists; the pane sits in `Status::Starting` until it
    /// does. The calling stack sees no repaint — the arrival notifies.
    pub(super) fn start_session(&mut self, resume: Option<String>, cx: &mut Context<Self>) {
        self.start_session_with_options(
            resume.map(|id| RecoveryIdentity::new(AgentKind::Claude, id)),
            false,
            |_, _, _| {},
            cx,
        )
    }

    /// `on_result` runs once the backend either came up or failed to, carrying
    /// whether it did. The spawn no longer answers that on the calling stack,
    /// so a caller that reports the outcome does it from there.
    pub(super) fn start_session_with_options(
        &mut self,
        recovery: Option<RecoveryIdentity>,
        preserve_thread_settings: bool,
        on_result: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
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

        // A pinned effort reaches the backend through the launch, so the
        // picker shows it from the first frame rather than the level the
        // agent would otherwise have used.
        if !preserve_thread_settings && let Some(effort) = self.profile_effort() {
            self.settings.effort = Some(effort);
        }

        let kind = self.kind;
        let name = kind.display();
        let cwd = self.cwd.clone();

        let caps = kind.caps();
        // A resume into a backend that replays its own thread controls keeps
        // them; anything else starts from the remembered picks. The reviewer is
        // seeded separately because a backend can replay the rest without it.
        self.seed_thread_defaults = !preserve_thread_settings
            && (recovery.is_none() || !caps.resume_restores_thread_settings);
        self.seed_approval_reviewer = !preserve_thread_settings
            && recovery.is_some()
            && caps.resume_restores_thread_settings
            && !caps.resume_restores_approval_reviewer;
        self.restore_thread_settings_on_ready =
            preserve_thread_settings.then(|| self.settings.clone());

        // Replacing a conversation must clear any running or unread state
        // associated with the previous backend before the new epoch can emit.
        cx.emit(AgentPaneEvent::Interrupted);
        self.session_epoch = next_session_epoch(self.session_epoch);
        // A replacement conversation names the tab again from its own opening
        // message; the previous one's subject no longer describes the tab.
        self.conversation_named = false;
        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;
        let epoch = self.session_epoch;

        let (tx, mut rx) = mpsc::unbounded::<Value>();
        let deliver = move |message| {
            let _ = tx.unbounded_send(message);
        };
        let mut launch = agent_launch(&self.profile);
        // A backend that builds its system prompt from the model it resolves at
        // launch would otherwise describe a different model than the one
        // serving the turns, because the pick would only reach the CLI
        // afterwards. The pane already knows the pick here: the profile
        // assigned it above, or it is the one remembered for this profile. A
        // tab with neither leaves the flag off and starts on the CLI's
        // configured model.
        if caps.model_baked_into_launch {
            launch.model = self.settings.model.clone().or_else(|| {
                self.stored_thread_settings(cx)
                    .and_then(|stored| stored.model.clone())
            });
        }
        // Env names only: the values can carry API keys.
        info!(
            "agent session start: profile=\"{}\", executable=\"{}\", model={:?}, env=[{}]",
            self.profile.name,
            launch.executable,
            launch.model,
            launch
                .env
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        // Process creation blocks for hundreds of milliseconds on Windows
        // (cmd.exe, then the CLI's own launcher), which is long enough to drop
        // frames if it runs on the UI thread. The pane already models the gap
        // as `Status::Starting` with no backend installed, so the spawn moves
        // to a background thread and the result arrives in a later update.
        self.status = Status::Starting;
        let spawned = cx
            .background_executor()
            .spawn(async move { Backend::spawn(kind, &launch, cwd, recovery, deliver) });

        cx.spawn(async move |this, cx| {
            let spawned = spawned.await;
            let started = this
                .update(cx, |this, cx| {
                    // A superseded start reports nothing: the newer one owns
                    // the pane's state, and this caller's outcome no longer
                    // describes what the pane is doing.
                    match this.install_started_session(spawned, epoch, name, cx) {
                        Some(started) => {
                            on_result(this, started, cx);
                            cx.notify();
                            started
                        }
                        None => false,
                    }
                })
                .unwrap_or(false);
            if !started {
                return;
            }

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
                    this.update_suspension = Some(UpdateSuspension::Failed(
                        i18n("agent-session-exited-before-restored").replace("{name}", name),
                    ));
                }
                this.palette.awaiting_command_turn = false;
                if !this.palette.command_queue.is_empty() {
                    this.palette.command_queue.clear();
                    this.set_command_feedback(
                        CommandFeedbackKind::Error,
                        i18n("agent-session-queued-cancelled-exited").replace("{name}", name),
                        cx,
                    );
                }
                this.publish_queued_user_messages(cx);
                this.finish_working(cx);
                this.push_item(
                    SessionItem::Error {
                        text: i18n("agent-session-exited").replace("{name}", name),
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    /// Take ownership of a backend that finished spawning, reporting whether it
    /// came up. `None` means a newer start superseded this one while the
    /// process was coming up; that leaves a live CLI behind, so the orphan is
    /// shut down rather than dropped.
    fn install_started_session(
        &mut self,
        spawned: Result<Backend, String>,
        epoch: u64,
        name: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        if !is_current_session_epoch(self.session_epoch, epoch) {
            if let Ok(mut orphan) = spawned {
                cx.background_executor()
                    .spawn(async move {
                        let _ = orphan.shutdown(Duration::from_secs(5), true);
                    })
                    .detach();
            }
            return None;
        }

        Some(match spawned {
            Ok(session) => {
                self.session = Some(session);
                true
            }
            Err(err) => {
                cx.emit(AgentPaneEvent::Interrupted);
                self.status = Status::Exited;
                self.palette.awaiting_command_turn = false;
                self.palette.command_queue.clear();
                self.queued_user_messages.clear();
                let turn = self.turn_seq;
                self.transcript.update(cx, |transcript, _| {
                    transcript.push_stamped(
                        turn,
                        SessionItem::Error {
                            text: i18n("agent-session-start-failed")
                                .replace("{name}", name)
                                .replace("{error}", &err),
                        },
                    );
                });
                false
            }
        })
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(crate) fn kind(&self) -> AgentKind {
        self.kind
    }

    /// Progress through the task list this conversation is working from, as
    /// completed items out of the total, for the workspace entry's bar.
    pub(crate) fn task_tally(&self, cx: &App) -> Option<(u32, u32)> {
        self.transcript.read(cx).task_tally()
    }

    /// The launch profile this pane runs, so a tab opened from one of its
    /// rows launches the same agent with the same configuration.
    pub(crate) fn profile(&self) -> &AgentProfile {
        &self.profile
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

    /// Ask the backend what to call this conversation. Claude's CLI names it
    /// with a model call on the whole prompt; Codex stores names but writes
    /// none of its own, so it is told `opening_line`.
    ///
    /// Asked again for every message until a name arrives, because a request
    /// can come back empty: Claude's CLI refuses to name a conversation from
    /// under ten characters, which a short opening question can be. The
    /// adapter drops a request made while one is outstanding, so the repeat
    /// costs nothing once one is on its way.
    fn request_conversation_title(
        &mut self,
        text: &str,
        opening_line: String,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.as_mut() else {
            return;
        };

        for event in session.request_title(text, opening_line) {
            self.apply_event(event, cx);
        }
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
                i18n("agent-session-rewind-blocks-send").to_string(),
                cx,
            );
            return false;
        }
        if self.palette.awaiting_command_turn {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-command-starting").to_string(),
                cx,
            );
            return false;
        }

        let settings = self.settings.clone();
        let scratch = scratch_dir(self.agent_route.as_str());
        let outcome = match self.session.as_mut() {
            Some(session) => {
                session.send_user_message(&text, &settings, skill, &self.attachments, &scratch)
            }
            None => SendOutcome::NotReady,
        };

        // Both refusals keep the composed text recoverable; they differ only in
        // whether the backend could say why.
        let refusal = match &outcome {
            SendOutcome::NotReady => {
                Some(i18n("agent-session-still-starting").replace("{name}", self.kind.display()))
            }
            SendOutcome::Rejected { message } => Some(message.clone()),
            SendOutcome::StartedTurn | SendOutcome::Steered => None,
        };
        if let Some(text) = refusal {
            self.push_item(SessionItem::Error { text }, cx);
            return false;
        }

        // Accepted: the images went with it, so the transcript keeps them and
        // the composer lets them go. A refusal above keeps them pending, so
        // the message stays as recoverable as its text.
        let sent_images: Vec<Arc<Image>> = self
            .attachments
            .iter()
            .map(|attachment| attachment.image())
            .collect();
        self.attachments.clear();

        // The first message commits this tab to its conversation; the
        // history list is no longer offered.
        self.history_ui.mode = RecentSessionsMode::Hidden;

        // A slash command names nothing — it instructs the CLI rather than
        // stating a subject, and the settings controls send some of them on
        // the user's behalf — so a conversation that opens with one waits for
        // the message that follows.
        if !self.conversation_named
            && let Some(opening_line) = tab_title_from_prompt(&text)
        {
            self.request_conversation_title(&text, opening_line, cx);
        }

        match outcome {
            SendOutcome::StartedTurn => {
                self.turn_seq += 1;
                let unanswered_prompt = restore_on_interrupt.then(|| UnansweredPrompt {
                    turn: self.turn_seq,
                    text: text.clone(),
                    skill: skill.cloned(),
                });
                self.push_item_with_images(
                    SessionItem::UserMessage { text: Some(text) },
                    sent_images,
                    cx,
                );
                self.unanswered_prompt = unanswered_prompt;
                self.start_working(cx);
            }
            SendOutcome::Steered => {
                // The backend may republish this row with an identity of its
                // own a moment later; until then it is this side's record that
                // the message is pending, and it carries no removal control.
                self.queued_user_messages
                    .push_back(QueuedPrompt::local(text));
                cx.notify();
            }
            SendOutcome::NotReady | SendOutcome::Rejected { .. } => unreachable!(),
        }

        true
    }

    pub(super) fn clear_conversation_presentation(&mut self, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |transcript, _| transcript.clear());
        self.turn_seq = 0;
        self.turn_submitted_at = None;
        self.first_output_latency = None;
        self.unanswered_prompt = None;
        // The new conversation restarts turn ids from zero, so a stop request
        // left over from the old one could match an unrelated future turn.
        self.pending_interrupt = None;
        self.context_window_usage = None;
        self.context_composition = None;
        self.goal = None;
        self.plan_mode = false;
        self.session_stats = None;
        self.queued_user_messages.clear();
        self.rewind.state = None;
        self.rewind.file_completion = None;
        self.history_ui.pending_resume_replay = None;
        // An approval belongs to the tool call that asked for it. The backend
        // that asked is the one being replaced, so leaving the card up offers a
        // decision that would be answered into a different conversation.
        self.pending_approval = None;
        // Child rows belong to the conversation being replaced; keeping them
        // would show another parent session's tasks until the new adapter
        // publishes its first snapshot.
        self.background_tasks = None;
        self.background_task_transcripts.clear();
        // Workflow runs are scoped the same way, and their refresh must not
        // keep polling a directory that belongs to the replaced conversation.
        self.clear_workflows();
        // The question card is answered into the backend being replaced, so it
        // cannot outlive it either.
        self.pending_questions = None;
    }

    /// Rebuild Claude child agents from the session's persisted history. The
    /// read runs on a background thread and its failure never blocks the
    /// parent transcript or composer.
    pub(in crate::agent_pane) fn restore_background_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self
            .session
            .as_ref()
            .and_then(|session| session.session_id())
            .map(str::to_owned)
        else {
            return;
        };
        if self.restored_task_session.as_deref() == Some(session_id.as_str()) {
            return;
        }
        self.restored_task_session = Some(session_id.clone());

        let Some(session) = self.session.as_mut() else {
            return;
        };
        // Captured before the read starts so live updates that land while it
        // runs keep their newer state.
        let starting_sequence = session.begin_task_restoration();
        let cwd = self.cwd.clone();
        let epoch = self.session_epoch;

        cx.spawn(async move |this, cx| {
            let restored = cx
                .background_executor()
                .spawn(async move { sessions::load_task_history(cwd.as_deref(), &session_id) })
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.session_epoch != epoch {
                    return;
                }
                let Some(session) = this.session.as_mut() else {
                    return;
                };
                for event in session.finish_task_restoration(restored, starting_sequence) {
                    this.apply_event(event, cx);
                }
            });
        })
        .detach();
    }

    /// Opening the `Background Tasks` view asks the provider for fresher data.
    pub(crate) fn refresh_background_tasks(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.refresh_background_tasks();
        }
    }

    /// Provider-qualified identity of the parent session child tasks belong to.
    /// `None` until the backend reports a thread or session id, which is what
    /// disables the title-bar `Background Tasks` button.
    pub(crate) fn background_task_parent(&self) -> Option<BackgroundTaskKey> {
        let identity = self.session.as_ref()?.recovery_identity()?;
        Some(match identity.kind {
            AgentKind::Codex => BackgroundTaskKey::codex(identity.id),
            AgentKind::Claude => BackgroundTaskKey::claude_code(identity.id),
            // Child agents are not mapped for DeepSeek yet, so there is no
            // parent to name and the Background Tasks button stays disabled.
            AgentKind::DeepSeek => return None,
        })
    }

    /// Ask the provider for one child's conversation. A provider that already
    /// has it, or that streams it live, does no work here.
    pub(crate) fn load_background_task_transcript(
        &mut self,
        key: &BackgroundTaskKey,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.cwd.clone();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        for event in session.load_background_task_transcript(key, cwd.as_deref()) {
            self.apply_event(event, cx);
        }
    }

    /// Stop one child agent, leaving this tab's own turn running. Reports
    /// whether the request was accepted, so the view can say so when a child
    /// turns out not to be stoppable after all — the snapshot a row was drawn
    /// from can be a moment behind the child finishing on its own.
    pub(crate) fn interrupt_background_task(&mut self, key: &BackgroundTaskKey) -> bool {
        self.session
            .as_mut()
            .is_some_and(|session| session.interrupt_background_task(key))
    }

    /// One child's conversation, only while the pane still holds the session
    /// that child belongs to.
    pub(crate) fn background_task_transcript(
        &self,
        key: &BackgroundTaskKey,
    ) -> Option<&BackgroundTaskTranscript> {
        self.background_tasks()?;
        self.background_task_transcripts.get(key)
    }

    /// The latest snapshot, only while it still describes the session the pane
    /// currently holds. A snapshot left over from a replaced session is hidden
    /// rather than shown against the new parent.
    pub(crate) fn background_tasks(&self) -> Option<&BackgroundTaskSnapshot> {
        scoped_background_tasks(
            self.background_task_parent().as_ref(),
            self.background_tasks.as_ref(),
        )
    }

    /// Child agents of this tab the provider currently reports as active.
    pub(crate) fn running_background_tasks(&self) -> usize {
        self.background_tasks()
            .map(BackgroundTaskSnapshot::active_count)
            .unwrap_or(0)
    }

    /// Child agents this tab has, running and finished alike. A finished child
    /// is still something to open the view for, so the chrome asks for this
    /// rather than the running count when deciding to offer the control.
    pub(crate) fn background_task_count(&self) -> usize {
        self.background_tasks()
            .map(|tasks| tasks.tasks.len())
            .unwrap_or(0)
    }

    pub(super) fn reset_conversation(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        // A fresh conversation always follows the live tail again, even if
        // the previous transcript was scrolled up when it was discarded.
        self.clear_conversation_presentation(cx);
        self.settings = ThreadSettings::default();
        self.models.clear();
        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;
        reset_command_runtime(
            !self.kind.caps().async_command_discovery,
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
        self.turn_submitted_at = Some(Instant::now());
        self.transcript
            .update(cx, |transcript, cx| transcript.start_working(cx));
        cx.notify();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let ticking = this.update(cx, |this, cx| {
                    if this.transcript.read(cx).is_working() {
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
        let turn = self.turn_seq;
        self.transcript
            .update(cx, |transcript, cx| transcript.settle_turn(turn, cx));
        self.note_response_settled(cx);
        cx.notify();
    }

    /// Stamp the moment the agent stopped answering and keep the composer's
    /// reading of it current.
    ///
    /// The label's resolution decides the cadence: a reading in seconds has to
    /// be redrawn every second, one in minutes only every minute. A pane whose
    /// last answer was an hour ago would otherwise hold the frame pump awake
    /// for a label that has not changed.
    fn note_response_settled(&mut self, cx: &mut Context<Self>) {
        let restart = self.last_response_at.is_none();
        self.last_response_at = Some(Instant::now());

        if !restart {
            return;
        }

        cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |this, cx| {
                    cx.notify();
                    this.last_response_at
                        .map(|at| response_age_tick(at.elapsed()))
                }) else {
                    break;
                };
                let Some(interval) = interval else {
                    break;
                };

                cx.background_executor().timer(interval).await;
            }
        })
        .detach();
    }

    fn note_visible_agent_output(&mut self) {
        // Only the first output of a turn answers "how long until it said
        // something", so taking the stamp both records the reading and closes
        // the measurement for the rest of the turn.
        if let Some(submitted_at) = self.turn_submitted_at.take() {
            self.first_output_latency = Some(submitted_at.elapsed());
        }

        if self
            .unanswered_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.turn == self.turn_seq)
        {
            self.unanswered_prompt = None;
        }
    }

    pub(super) fn interrupt_from_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let working = self.transcript.read(cx).is_working();
        if working {
            self.pending_interrupt = Some(self.turn_seq);
        }
        if let Some(prompt) = self
            .unanswered_prompt
            .take()
            .filter(|prompt| prompt.turn == self.turn_seq && working)
        {
            let turn = prompt.turn;
            self.transcript
                .update(cx, |transcript, cx| transcript.discard_turn(turn, cx));

            let current = self.input.read(cx).text().to_string();
            let restored = restored_input_after_interruption(&prompt.text, &current);
            let cursor = restored.len();
            self.input.update(cx, |input, cx| {
                input.set_value(restored, window, cx);
                input.set_selected_range(cursor..cursor, cx);
            });
            self.palette.skill_binding = prompt.skill;
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

    /// Record a pick without answering yet; the card stays open until the user
    /// submits, so multi-select questions can accumulate choices.
    pub(super) fn toggle_question_option(
        &mut self,
        question: usize,
        option: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(prompt) = self.pending_questions.as_mut() {
            prompt.toggle(question, option);
            // Clicking also moves the highlight, so a switch to the keyboard
            // continues from the option the user just touched rather than from
            // wherever the arrows were left.
            prompt.focus = (question, option);
            cx.notify();
        }
    }

    /// Drive the question card from the keyboard. Returns whether the card
    /// consumed the key, so the caller can fall through to the surfaces that
    /// share these keys when no card is up.
    ///
    /// Enter answers the highlighted option rather than submitting the card:
    /// with several questions, or a multi-select one, the user is rarely done
    /// after one press, and a key that sometimes submits and sometimes selects
    /// cannot be predicted from what is on screen.
    pub(in crate::agent_pane) fn handle_question_control(
        &mut self,
        control: PaletteControl,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(prompt) = self.pending_questions.as_mut() else {
            return false;
        };

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                if prompt.move_focus(matches!(control, PaletteControl::Next)) {
                    cx.stop_propagation();
                    cx.notify();
                    return true;
                }
                false
            }
            PaletteControl::Activate => {
                let (question, option) = prompt.focus;
                cx.stop_propagation();
                self.toggle_question_option(question, option, cx);
                true
            }
            // Completion belongs to the composer, and dismissing the card would
            // answer the question by refusing it, which needs the visible
            // control rather than a keystroke.
            PaletteControl::Complete | PaletteControl::Dismiss => false,
        }
    }

    /// Submit the current picks, or decline when `submit` is false. The card is
    /// dismissed immediately; the session's `QuestionsResolved` confirmation is
    /// then an idempotent status refresh, as with approvals.
    pub(super) fn respond_questions(&mut self, submit: bool, cx: &mut Context<Self>) {
        let Some(prompt) = self.pending_questions.take() else {
            return;
        };

        let answers = (submit && prompt.is_complete()).then(|| prompt.answers());

        self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

        if let Some(session) = self.session.as_mut() {
            session.respond_questions(answers);
        }
        cx.notify();
    }

    /// Push the current model and effort picks to a harness that applies them
    /// as their own request.
    ///
    /// A refusal restores both pickers from what the session is actually set
    /// to, because a picker left showing a value the harness never adopted
    /// would misreport which model the next turn runs on.
    pub(super) fn apply_model_selection(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self.settings.model.clone() else {
            return;
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };

        // The levels belong to the exact model route, so an effort picked for
        // the previous model could be one the new route rejects. A model change
        // therefore travels alone and lets the adapter apply its own default,
        // which is also why no caller has to clear the effort itself.
        let effort = (session.selection().0 == Some(model.as_str()))
            .then(|| self.settings.effort.clone())
            .flatten();

        // Seeding the pickers from a remembered pick reaches here too, and it
        // usually names what the session already has.
        if session.selection() == (Some(model.as_str()), effort.as_deref()) {
            return;
        }

        let outcome = session.select_model(&model, effort.as_deref());

        // Whether it was taken or refused, the pickers now show what the
        // session is actually set to: the harness answers a model change with
        // the effort it chose, and a refusal leaves the previous pair standing.
        let (model, effort) = session.selection();
        self.settings.model = model.map(str::to_string);
        self.settings.effort = effort.map(str::to_string);

        match outcome {
            Ok(()) => cx.notify(),
            Err(error) => self.set_command_feedback(CommandFeedbackKind::Error, error, cx),
        }
    }

    /// Resume the picked history entry without discarding the visible
    /// conversation until the target confirms it can be opened.
    pub(super) fn resume_session(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(summary) = self.history_ui.sessions.get(index) else {
            return;
        };
        let id = summary.id.clone();
        let elsewhere = summary
            .cwd
            .clone()
            .filter(|cwd| !directories_match(Some(cwd.as_str()), self.cwd.as_deref()));

        if self.history_ui.mode == RecentSessionsMode::Loading {
            return;
        }

        // A conversation belongs to the directory it ran in. Continuing one
        // from elsewhere in this tab would either fail to find it or run it
        // against the wrong tree, so it opens where it worked instead.
        if let Some(cwd) = elsewhere {
            self.history_ui.selected = index;
            cx.emit(AgentPaneEvent::ResumeElsewhere {
                cwd,
                session_id: id,
            });
            cx.notify();
            return;
        }

        let previous_status = self.status;
        self.history_ui.mode = RecentSessionsMode::Loading;
        self.history_ui.selected = index;
        self.history_ui.pending_resume_replay = None;
        self.status = Status::Starting;
        // A backend that replays the resumed conversation's controls owns them;
        // otherwise they stay local profile preferences. The reviewer is seeded
        // separately because a backend can replay the rest without it.
        let caps = self.kind.caps();
        self.seed_thread_defaults = !caps.resume_restores_thread_settings;
        self.seed_approval_reviewer =
            caps.resume_restores_thread_settings && !caps.resume_restores_approval_reviewer;
        self.set_command_feedback(
            CommandFeedbackKind::Notice,
            i18n("agent-session-opening-recent").to_string(),
            cx,
        );

        match self.kind {
            // Both continue an earlier conversation inside the running session,
            // and both answer whether the request reached a backend that could.
            AgentKind::Codex | AgentKind::DeepSeek => {
                if !self
                    .session
                    .as_mut()
                    .is_some_and(|session| session.resume_thread(&id))
                {
                    self.history_ui.mode = RecentSessionsMode::Open;
                    self.status = previous_status;
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        i18n("agent-session-codex-recent-not-ready").to_string(),
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

                        this.start_session_with_options(
                            Some(RecoveryIdentity::new(AgentKind::Claude, id)),
                            false,
                            move |this, started, _| {
                                if started {
                                    this.history_ui.pending_resume_replay = Some(replay);
                                } else {
                                    this.history_ui.mode = RecentSessionsMode::Open;
                                }
                            },
                            cx,
                        );
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

    /// The picks remembered for this profile, falling back to the bucket its
    /// agent kind shares with unnamed profiles.
    pub(super) fn stored_thread_settings<'a>(&self, cx: &'a App) -> Option<&'a ThreadSettings> {
        let defaults = cx.try_global::<AgentThreadDefaults>()?;
        defaults
            .0
            .get(&self.defaults_key())
            .or_else(|| defaults.0.get(self.kind.id()))
    }

    /// Effective startup model after protocol mapping and user environment
    /// overrides. Claude resolves `ANTHROPIC_MODEL` with last-value-wins
    /// semantics; Codex receives the profile field over app-server RPC.
    pub(super) fn profile_model(&self) -> Option<String> {
        let launch = agent_launch(&self.profile);
        match self.kind {
            AgentKind::Claude => launch_env_value(&launch, ANTHROPIC_MODEL_ENV),
            AgentKind::Codex => launch.model,
            // DeepSeek takes a model through a per-session call rather than the
            // launch, and that call is not mapped yet, so the profile field
            // cannot describe what the session will actually run.
            AgentKind::DeepSeek => None,
        }
    }

    /// The reasoning effort this pane's profile pins. Claude receives it as a
    /// launch flag and Codex as a thread-start parameter; the picker shows it
    /// either way.
    pub(super) fn profile_effort(&self) -> Option<String> {
        agent_launch(&self.profile).effort
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
