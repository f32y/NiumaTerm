use futures::StreamExt as _;
use gpui::prelude::*;
use nmt_agent_utils::AgentEvent;

use crate::UnansweredPrompt;
use crate::pane_state::{ChildAgents, SessionRuntime, TurnState};
use crate::thread_controls::{ThreadControls, launch_effort, launch_model, stored_thread_settings};
use crate::view::session_state::SessionStateBadge;
mod backend;
mod background_tasks;
mod conversation;
mod events;
mod history;
#[cfg(test)]
mod tests;
mod turn;
mod update_recovery;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use std::{env, fs};

use futures::channel::mpsc;
use gpui::{App, Context, Image, Window};
use gpui_component::input::{InputEvent, InputState};
use nmt_agent_utils::chat::{
    Item as SessionItem, QueuedPrompt, SendOutcome, SkillReference, ThreadSettings,
};
use nmt_agent_utils::codex::app_server;
use nmt_agent_utils::{
    AgentEventKind, AgentRoute, AgentWorkspace, agent_process, git, normalize_body, normalize_title,
};
use nmt_config::profile::{AgentProfile, AgentProfileKind};
use nmt_i18n::i18n;
use serde_json::Value;
use tracing::info;

use crate::capabilities::QueuedPromptDelivery;
use crate::commands::{
    is_current_session_epoch, next_session_epoch, reconcile_skill_binding, reset_command_runtime,
};
use crate::composer::attachments::{ComposerAttachments, scratch_dir};
use crate::composer::{CommandFeedbackKind, ForkFlow, prompt_with_response_annotations};
use crate::input_history::{InputHistoryNavigation, InputHistoryScope};
use crate::profile::{AgentKind, agent_launch};
pub(super) use crate::session::backend::Backend;
use crate::session::backend::ConversationTitleRequest;
pub use crate::session::backend::RecoveryIdentity;
#[cfg(test)]
pub(crate) use crate::session::backend::TestBackend;
pub(super) use crate::session::update_recovery::UpdateSuspension;
pub use crate::session::update_recovery::{
    RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};
use crate::settings::AgentSettings;
use crate::transcript::TranscriptView;
use crate::workflows::WorkflowUi;
use crate::{
    AgentPane, AgentPaneEvent, GitBranchPoll, RecentSessionsMode, RewindFlow, SessionHistoryUi,
    SlashPalette,
};

/// A pane's attachment files live only as long as the pane: the harness that
/// reads them has already read what it was sent, and nothing else refers to
/// them. A directory that was never created removes cleanly.
impl Drop for AgentPane {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(scratch_dir(self.agent_route.as_str()));
    }
}

/// How long a start is allowed to run before the tab is covered. Long enough
/// that a reused host, which answers in a frame or two, never shows an overlay
/// at all; short enough that a cold start is explained rather than looking like
/// a dead tab.
const START_OVERLAY_DELAY: Duration = Duration::from_millis(400);

/// Whether two recorded working directories name the same place. Compared
/// case-insensitively with separators normalized, because the two sides come
/// from different writers: one from the tab's own configuration, the other
/// from whatever the agent recorded when it ran.
pub(crate) fn directories_match(left: Option<&str>, right: Option<&str>) -> bool {
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
/// The pane's branch label: a detached `HEAD` shows its short commit,
/// matching the git footer's presentation of the same state.
fn branch_label(cwd: &str, max_age: Duration) -> Option<String> {
    Some(match git::current_branch(cwd, max_age)? {
        git::CheckedOut::Branch(branch) => branch,
        git::CheckedOut::Detached(commit) => {
            i18n("git-status-detached").replace("{commit}", &commit)
        }
    })
}

pub(crate) fn directory_label(cwd: &str) -> String {
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

fn conversation_title_request(kind: AgentKind, text: &str) -> Option<ConversationTitleRequest> {
    let provisional_title = match kind {
        AgentKind::Codex => app_server::provisional_title_from_prompt(text),
        AgentKind::Claude | AgentKind::DeepSeek => tab_title_from_prompt(text),
    }?;
    Some(ConversationTitleRequest {
        description: text.to_string(),
        provisional_title,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Starting,
    Idle,
    Running,
    Exited,
}

impl AgentPane {
    pub fn new(
        profile: AgentProfile,
        workspace: AgentWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_resuming(profile, workspace, None, window, cx)
    }

    /// A pane whose first session continues `resume` instead of opening a
    /// fresh conversation. Used to reopen a listed conversation in the
    /// directory it ran in, which is a different one than the tab that
    /// listed it.
    pub fn new_resuming(
        profile: AgentProfile,
        workspace: AgentWorkspace,
        resume: Option<RecoveryIdentity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let kind = AgentKind::from_profile(profile.kind);
        let cwd = workspace.primary().map(str::to_string);
        let input_history_scope = InputHistoryScope::local(kind, &workspace);
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

        // The rows that branch or rewind the conversation address the pane, so
        // the pane's own transcript is told which pane it belongs to; a view
        // mirroring somebody else's conversation is left without one.
        let owner = cx.entity().downgrade();
        let transcript = cx.new(|_| {
            let mut transcript = TranscriptView::new(kind, cwd.clone());
            transcript.set_owner(owner);
            transcript
        });

        let mut this = Self {
            focus: cx.focus_handle(),
            agent_route: agent_process().allocate_route(),
            kind,
            profile,
            // Nothing has started yet, so the active snapshot matches the
            // configured list until the first conversation clones it.
            active_workspace: workspace.clone(),
            workspace,
            input_history_scope,
            input_history_navigation: InputHistoryNavigation::default(),
            attachments: ComposerAttachments::default(),
            last_response_at: None,
            conversation_named: false,
            pending_conversation_rename: None,
            transcript,
            input,
            runtime: SessionRuntime {
                backend: None,
                epoch: 0,
                status: Status::Starting,
                start_failure: None,
                start_overlay_due: false,
                update_suspension: None,
                last_recovery_snapshot: None,
            },
            history_ui: SessionHistoryUi::default(),
            pending_approval: None,
            pending_questions: None,
            controls: ThreadControls {
                settings: ThreadSettings::default(),
                seed_thread_defaults: true,
                seed_approval_reviewer: false,
                restore_on_ready: None,
                models: Vec::new(),
                approval_presets: Vec::new(),
                agent_presets: Vec::new(),
                agent_preset: None,
                effort_drag: None,
            },
            turn: TurnState {
                seq: 0,
                submitted_at: None,
                first_output_latency: None,
                unanswered_prompt: None,
                pending_interrupt: None,
                queued_user_messages: VecDeque::new(),
                published_prompt: None,
            },
            palette: SlashPalette {
                provider_commands_ready: !kind.caps().async_command_discovery,
                ..SlashPalette::default()
            },
            rewind: RewindFlow::default(),
            fork: ForkFlow::default(),
            git_branch_poll: GitBranchPoll::default(),
            context_window_usage: None,
            context_composition: None,
            session_state: SessionStateBadge::default(),
            session_stats: None,
            children: ChildAgents {
                background_tasks: None,
                transcripts: HashMap::new(),
                restored_session: None,
            },
            workflows: WorkflowUi::default(),
        };

        this.start_session_with_options(resume, false, |_, _, _| {}, cx);
        this.refresh_git_branch(cx);

        cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |_, cx| {
                    cx.global::<AgentSettings>().git_status_refresh_interval
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

    pub fn agent_route(&self) -> &AgentRoute {
        &self.agent_route
    }

    pub fn agent_kind(&self) -> AgentKind {
        self.kind
    }

    /// The tab's primary directory, which transcript links resolve against.
    pub fn working_directory(&self) -> Option<String> {
        self.cwd()
    }

    /// The tab's primary directory: where its process runs, what its provider
    /// session history is scoped to, and what a relative path resolves against.
    pub(crate) fn cwd(&self) -> Option<String> {
        self.workspace.primary().map(str::to_string)
    }

    /// The directories this tab is currently configured with. A conversation
    /// started from now on receives these.
    pub(crate) fn configured_workspace(&self) -> &AgentWorkspace {
        &self.workspace
    }

    /// The directories the running conversation was started with.
    #[cfg(test)]
    pub(crate) fn active_workspace(&self) -> &AgentWorkspace {
        &self.active_workspace
    }

    /// Replace the configured directory list after the parent workspace was
    /// edited. The running conversation keeps the snapshot it started with;
    /// the next one clones this.
    pub fn set_workspace(&mut self, workspace: AgentWorkspace, cx: &mut Context<Self>) {
        if self.workspace == workspace {
            return;
        }
        self.input_history_scope = InputHistoryScope::local(self.kind, &workspace);
        self.workspace = workspace;
        cx.notify();
    }

    /// Append one item to the conversation, tagged with the current turn so
    /// settled turns fold as one unit.
    pub(crate) fn push_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        self.push_item_with_images(item, Vec::new(), cx);
    }

    /// Append an item along with the images it carried, which only a sent
    /// user message has.
    pub(crate) fn push_item_with_images(
        &mut self,
        item: SessionItem,
        images: Vec<Arc<Image>>,
        cx: &mut Context<Self>,
    ) {
        let turn = self.turn.seq;
        self.transcript
            .update(cx, |transcript, cx| transcript.push(turn, item, images, cx));
        cx.notify();
    }

    pub(super) fn refresh_git_branch(&mut self, cx: &mut Context<Self>) {
        if !self.git_branch_poll.begin_refresh() {
            return;
        }

        let Some(cwd) = self.cwd().or_else(|| {
            env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        }) else {
            self.git_branch_poll.complete(None);
            return;
        };

        // Every tab open on this directory asks the same question on the same
        // interval, so an answer read within one is theirs to share.
        let max_age = Duration::from_secs(
            cx.global::<AgentSettings>()
                .git_status_refresh_interval
                .max(1),
        );
        let fetch = cx
            .background_executor()
            .spawn(async move { branch_label(&cwd, max_age) });

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
            session_id: format!("agent-tab-{}", self.runtime.epoch),
            turn_id: (kind != AgentEventKind::SessionStarted)
                .then(|| format!("turn-{}", self.turn.seq)),
            kind,
            title: normalize_title(title),
            body: normalize_body(body),
        }));
    }

    pub(super) fn latest_agent_message(&self, cx: &App) -> Option<String> {
        self.transcript
            .read(cx)
            .latest_agent_message(self.turn.seq)
            .map(str::to_owned)
    }

    /// Request the backend process (optionally resuming a persisted Claude
    /// session) and pump its messages onto the UI thread. Channel closure is
    /// the EOF signal (the sender is owned by the reader thread). Returns
    /// before the process exists; the pane sits in `Status::Starting` until it
    /// does. The calling stack sees no repaint — the arrival notifies.
    /// Whether this pane covers its own start at all.
    ///
    /// The two harnesses that take long enough to be worth explaining: the
    /// DeepSeek host is a Node process that may still be fetching its package,
    /// and the Codex app server reads its own configuration and catalogs
    /// before it answers. Claude's CLI is up within a frame or two, where an
    /// overlay would read as a flicker.
    ///
    /// Every pane holds the cover back for a moment either way, so a start
    /// that lands quickly is never covered whichever harness it is.
    pub(crate) fn wears_start_overlay(&self) -> bool {
        matches!(self.kind, AgentKind::Codex | AgentKind::DeepSeek)
    }

    /// Whether the cover is on screen right now.
    ///
    /// Read from the start's own state rather than latched on and off around
    /// it. A harness's process exists well before the harness answers: Codex
    /// spawns in a moment and then reads its configuration and catalogs, and
    /// the pane stays in `Status::Starting` until the thread-ready message
    /// arrives. Tying the cover to the spawn instead put it on screen after
    /// the process was already up and left it there once the harness was
    /// ready.
    pub(super) fn shows_start_overlay(&self) -> bool {
        self.wears_start_overlay()
            && self.runtime.start_overlay_due
            && self.runtime.status == Status::Starting
    }

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
            .global::<AgentSettings>()
            .profiles
            .iter()
            .find(|p| p.kind == self.profile.kind && p.name == self.profile.name)
        {
            self.profile = fresh.clone();
        }

        // The profile model is known before either CLI completes its
        // handshake, so the picker need not flash the backend default while a
        // custom endpoint is starting.
        if !preserve_thread_settings && let Some(model) = launch_model(self.kind, &self.profile) {
            self.controls.settings.model = Some(model);
        }

        // A pinned effort reaches the backend through the launch, so the
        // picker shows it from the first frame rather than the level the
        // agent would otherwise have used.
        if !preserve_thread_settings && let Some(effort) = launch_effort(&self.profile) {
            self.controls.settings.effort = Some(effort);
        }

        let kind = self.kind;
        let name = kind.display();
        // The conversation about to start owns this snapshot for its whole
        // life; a later workspace edit reaches the one after it.
        self.active_workspace = self.workspace.clone();
        let workspace = self.active_workspace.clone();

        let caps = kind.caps();
        // A resume into a backend that replays its own thread controls keeps
        // them; anything else starts from the remembered picks. The reviewer is
        // seeded separately because a backend can replay the rest without it.
        self.controls.seed_thread_defaults = !preserve_thread_settings
            && (recovery.is_none() || !caps.resume_restores_thread_settings);
        self.controls.seed_approval_reviewer = !preserve_thread_settings
            && recovery.is_some()
            && caps.resume_restores_thread_settings
            && !caps.resume_restores_approval_reviewer;
        self.controls.restore_on_ready =
            preserve_thread_settings.then(|| self.controls.settings.clone());

        if let Some(session) = self.runtime.backend.as_mut() {
            session.cancel_title_generation();
        }

        // Replacing a conversation must clear any running or unread state
        // associated with the previous backend before the new epoch can emit.
        cx.emit(AgentPaneEvent::Interrupted);
        // The previous attempt's reason describes a backend nobody is waiting
        // on any more, and this start is what the pane now reports.
        self.runtime.start_failure = None;
        self.runtime.epoch = next_session_epoch(self.runtime.epoch);
        // A replacement conversation names the tab again from its own opening
        // message; the previous one's subject no longer describes the tab.
        self.conversation_named = false;
        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;
        let epoch = self.runtime.epoch;

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
            launch.model = self.controls.settings.model.clone().or_else(|| {
                stored_thread_settings(self.kind, &self.profile, cx)
                    .and_then(|stored| stored.model.clone())
            });
        }
        let codex_host_catalog = if kind == AgentKind::Codex {
            cx.global::<AgentSettings>()
                .profiles
                .iter()
                .filter(|profile| profile.kind == AgentProfileKind::Codex)
                .map(agent_launch)
                .collect()
        } else {
            Vec::new()
        };
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
        self.runtime.status = Status::Starting;
        // A host that is already running answers within a frame or two, so the
        // cover is held back rather than shown and pulled away as a flicker.
        // Nothing repaints while the start runs, so the hold has to wake the
        // pane itself instead of being read from a clock at render time.
        self.runtime.start_overlay_due = false;
        if self.wears_start_overlay() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(START_OVERLAY_DELAY).await;
                let _ = this.update(cx, |this, cx| {
                    if this.runtime.status == Status::Starting {
                        this.runtime.start_overlay_due = true;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        let spawned = cx.background_executor().spawn(async move {
            Backend::spawn(
                kind,
                &launch,
                &codex_host_catalog,
                &workspace,
                recovery,
                deliver,
            )
        });

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
                    if !is_current_session_epoch(this.runtime.epoch, epoch) {
                        return false;
                    }

                    let events = match this.runtime.backend.as_mut() {
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
                if !is_current_session_epoch(this.runtime.epoch, epoch) {
                    return;
                }
                let exit_events = this
                    .runtime
                    .backend
                    .as_mut()
                    .map(Backend::process_exit)
                    .unwrap_or_default();
                for event in exit_events {
                    this.apply_event(event, cx);
                }
                cx.emit(AgentPaneEvent::Interrupted);
                this.runtime.status = Status::Exited;
                if matches!(
                    this.runtime.update_suspension,
                    Some(UpdateSuspension::Reconnecting)
                ) {
                    this.runtime.update_suspension = Some(UpdateSuspension::Failed(
                        i18n("agent-session-exited-before-restored").replace("{name}", name),
                    ));
                }
                this.palette.awaiting_command_turn = false;
                if !this.palette.command_queue.is_empty() {
                    this.palette.command_queue.clear();
                    this.palette.set_feedback(
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
        if !is_current_session_epoch(self.runtime.epoch, epoch) {
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
                self.runtime.backend = Some(session);
                true
            }
            Err(err) => {
                cx.emit(AgentPaneEvent::Interrupted);
                self.runtime.status = Status::Exited;
                self.palette.awaiting_command_turn = false;
                self.palette.command_queue.clear();
                self.turn.queued_user_messages.clear();
                let text = i18n("agent-session-start-failed")
                    .replace("{name}", name)
                    .replace("{error}", &err);
                self.runtime.start_failure = Some(text.clone());
                let turn = self.turn.seq;
                self.transcript.update(cx, |transcript, _| {
                    transcript.push_stamped(turn, SessionItem::Error { text });
                });
                false
            }
        })
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn kind(&self) -> AgentKind {
        self.kind
    }

    /// Progress through the task list this conversation is working from, as
    /// completed items out of the total, for the workspace entry's bar.
    pub fn task_tally(&self, cx: &App) -> Option<(u32, u32)> {
        self.transcript.read(cx).task_tally()
    }

    /// The launch profile this pane runs, so a tab opened from one of its
    /// rows launches the same agent with the same configuration.
    pub fn profile(&self) -> &AgentProfile {
        &self.profile
    }

    /// Name of the launch profile, persisted with the tab snapshot so
    /// restore reopens the same profile.
    pub fn profile_name(&self) -> &str {
        &self.profile.name
    }

    /// Send one user message through the session with full turn bookkeeping;
    /// also used for UI-generated messages such as the `/effort` command.
    /// Returns false when the session isn't ready yet.
    pub(super) fn send_text(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        self.send_text_inner(text, None, None, cx)
    }

    pub(super) fn send_text_with_skill(
        &mut self,
        text: String,
        skill: Option<&SkillReference>,
        cx: &mut Context<Self>,
    ) -> bool {
        let response_annotations = self.attachments.annotations().to_vec();
        let submitted = prompt_with_response_annotations(&text, &response_annotations);
        self.send_text_inner(submitted, skill, Some((text, response_annotations)), cx)
    }

    fn send_text_inner(
        &mut self,
        text: String,
        skill: Option<&SkillReference>,
        restore_on_interrupt: Option<(String, Vec<String>)>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.branch_flow_holds_composer() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-rewind-blocks-send").to_string(),
                cx,
            );
            return false;
        }
        if self.palette.awaiting_command_turn {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-command-starting").to_string(),
                cx,
            );
            return false;
        }

        let title_text = restore_on_interrupt
            .as_ref()
            .map_or(text.as_str(), |(prompt, _)| prompt.as_str());
        let title_request = if self.conversation_named {
            None
        } else {
            conversation_title_request(self.kind, title_text)
        };
        let settings = self.controls.settings.clone();
        let scratch = scratch_dir(self.agent_route.as_str());
        let outcome = match self.runtime.backend.as_mut() {
            Some(session) if let Some(title) = title_request.as_ref() => session
                .send_user_message_with_title(
                    &text,
                    &settings,
                    skill,
                    self.attachments.images(),
                    &scratch,
                    title,
                ),
            Some(session) => session.send_user_message(
                &text,
                &settings,
                skill,
                self.attachments.images(),
                &scratch,
            ),
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

        if self.kind == AgentKind::Codex
            && let Some(title) = title_request
        {
            self.conversation_named = true;
            cx.emit(AgentPaneEvent::TitleSuggested(title.provisional_title));
        }

        // Accepted: the images went with it, so the transcript keeps them and
        // the composer lets them go. A refusal above keeps them pending, so
        // the message stays as recoverable as its text.
        let sent_images: Vec<Arc<Image>> = self
            .attachments
            .images()
            .iter()
            .map(|attachment| attachment.image())
            .collect();
        self.attachments.clear_images();
        if restore_on_interrupt.is_some() {
            self.attachments.clear_annotations();
        }

        // The first message commits this tab to its conversation; the
        // history list is no longer offered.
        self.history_ui.mode = RecentSessionsMode::Hidden;

        match outcome {
            SendOutcome::StartedTurn => {
                self.turn.seq += 1;
                // A backend that publishes its pending inbox lists this prompt
                // until the turn claims it; the row below is the claim's, so
                // the claim has to know it was already drawn.
                if self.kind.caps().queued_prompt_delivery == QueuedPromptDelivery::PendingInbox {
                    self.turn.published_prompt = Some(text.clone());
                }
                let unanswered_prompt =
                    restore_on_interrupt.map(|(text, response_annotations)| UnansweredPrompt {
                        turn: self.turn.seq,
                        text,
                        response_annotations,
                        skill: skill.cloned(),
                    });
                self.push_item_with_images(
                    SessionItem::UserMessage { text: Some(text) },
                    sent_images,
                    cx,
                );
                self.turn.unanswered_prompt = unanswered_prompt;
                self.start_working(cx);
            }
            SendOutcome::Steered => {
                // The backend may republish this row with an identity of its
                // own a moment later; until then it is this side's record that
                // the message is pending, and it carries no removal control.
                self.turn
                    .queued_user_messages
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
        self.turn.seq = 0;
        self.turn.submitted_at = None;
        self.turn.published_prompt = None;
        self.turn.first_output_latency = None;
        // The reading answers "how long has this conversation been waiting on
        // me"; the replaced conversation's last answer says nothing about the
        // fresh one, which has never been answered at all.
        self.last_response_at = None;
        self.turn.unanswered_prompt = None;
        // The new conversation restarts turn ids from zero, so a stop request
        // left over from the old one could match an unrelated future turn.
        self.turn.pending_interrupt = None;
        self.context_window_usage = None;
        self.context_composition = None;
        self.session_state.clear();
        self.session_stats = None;
        self.turn.queued_user_messages.clear();
        self.rewind.state = None;
        self.rewind.file_completion = None;
        self.fork.state = None;
        self.history_ui.pending_resume_replay = None;
        // An approval belongs to the tool call that asked for it. The backend
        // that asked is the one being replaced, so leaving the card up offers a
        // decision that would be answered into a different conversation.
        self.pending_approval = None;
        // Child rows belong to the conversation being replaced; keeping them
        // would show another parent session's tasks until the new adapter
        // publishes its first snapshot.
        self.children.background_tasks = None;
        self.children.transcripts.clear();
        // Workflow runs are scoped the same way, and their refresh must not
        // keep polling a directory that belongs to the replaced conversation.
        self.clear_workflows();
        // The question card is answered into the backend being replaced, so it
        // cannot outlive it either.
        self.pending_questions = None;
    }

    /// Opening the `Background Tasks` view asks the provider for fresher data.
    /// Pass a tab rename through to the conversation, so the name reaches the
    /// harness's own session record rather than living only in this tab.
    pub fn rename_session(&mut self, title: &str) {
        self.conversation_named = true;
        let can_address_conversation = self
            .runtime
            .backend
            .as_ref()
            .and_then(Backend::recovery_identity)
            .is_some();
        if can_address_conversation && let Some(session) = self.runtime.backend.as_mut() {
            session.rename_session(title);
            self.pending_conversation_rename = None;
        } else {
            self.pending_conversation_rename = Some(title.to_string());
        }
    }

    pub(super) fn reset_conversation(&mut self, cx: &mut Context<Self>) {
        // DeepSeek and Codex hosts stop once their final session reference is
        // released. Keeping the retired backend until the replacement starts
        // transfers that reference without restarting an unchanged host.
        if let Some(session) = self.runtime.backend.as_mut() {
            session.cancel_title_generation();
        }
        let retiring = self.runtime.backend.take();
        // A fresh conversation always follows the live tail again, even if
        // the previous transcript was scrolled up when it was discarded.
        self.clear_conversation_presentation(cx);
        self.controls.settings = ThreadSettings::default();
        self.controls.models.clear();
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
        self.palette.catalog = None;
        self.palette.feedback = None;
        self.history_ui.mode = RecentSessionsMode::Hidden;
        // The discarded conversation's subject no longer describes this tab,
        // so the tab falls back to its profile name until the replacement
        // conversation names itself.
        cx.emit(AgentPaneEvent::TitleSuggested(String::new()));

        // History records belong to the provider and remain intact; only the
        // live backend and this tab's conversation presentation are reset.
        self.start_session_with_options(None, false, move |_, _, _| drop(retiring), cx);
    }
}
