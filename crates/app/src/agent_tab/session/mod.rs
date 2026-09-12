use gpui::prelude::*;
use nmt_agent::session::ImageAttachment;
use nmt_agent::session::controller::SubmissionBlock;
use nmt_agent::session::delivery::{RecoverablePrompt, Submission};
#[cfg(test)]
use nmt_agent::session::naming::conversation_title_request as build_title_request;
pub(in crate::agent_tab) use nmt_agent::session::restore::directories_match;
use nmt_agent::transcript::conversation::ConversationImage;

use crate::agent_tab::capabilities::AgentCapabilities as _;
use crate::agent_tab::execution::{AgentSession, PresentationEffect, SessionOwner};
use crate::agent_tab::pane_state::TurnPresentation;
use crate::agent_tab::profile::AgentKindExt as _;
use crate::agent_tab::session::prompts::PendingPrompts;
use crate::agent_tab::thread_controls::ThreadControls;
use crate::agent_tab::view::session_state::SessionStateBadge;

mod background_tasks;
mod conversation;
pub(in crate::agent_tab) mod errors;
mod events;
pub(in crate::agent_tab) mod history;
pub(in crate::agent_tab) mod prompts;
mod startup;
#[cfg(test)]
mod tests;
pub(in crate::agent_tab) mod turn;
mod update_recovery;

use std::env;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Context, Entity, Image, Window};
use gpui_component::input::{InputEvent, TextareaState};
use nmt_agent::chat::{Item as SessionItem, SkillReference};
pub(super) use nmt_agent::session::Backend;
#[cfg(test)]
use nmt_agent::session::ConversationTitleRequest;
pub use nmt_agent::session::RecoveryIdentity;
pub use nmt_agent::session::lifecycle::{
    RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};
pub(super) use nmt_agent::session::lifecycle::{Status, UpdateSuspension};
#[cfg(test)]
pub(in crate::agent_tab) use nmt_agent::session::test_support::TestBackend;
use nmt_agent::{AgentEventKind, AgentRoute, AgentWorkspace, git};
use nmt_config::profile::AgentProfile;
use rust_i18n::t;

use crate::agent_tab::commands::reconcile_skill_binding;
use crate::agent_tab::composer::attachments::{ComposerAttachments, scratch_dir};
use crate::agent_tab::composer::{
    BranchFlow, CommandFeedbackKind, prompt_with_response_annotations,
};
use crate::agent_tab::fade::Fade;
use crate::agent_tab::input_history::{InputHistoryNavigation, InputHistoryScope};
use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::workflows::WorkflowUi;
use crate::agent_tab::{
    AgentPane, AgentPaneEvent, GitBranchPoll, RecentSessionsMode, SessionHistoryUi, SlashPalette,
};

/// The pane's branch label: a detached `HEAD` shows its short commit,
/// matching the git footer's presentation of the same state.
fn branch_label(cwd: &str, max_age: Duration) -> Option<String> {
    Some(match git::current_branch(cwd, max_age)? {
        git::CheckedOut::Branch(branch) => branch,

        git::CheckedOut::Detached(commit) => {
            t!("git-status-detached", commit = &commit).into_owned()
        }
    })
}

pub(in crate::agent_tab) fn directory_label(cwd: &str) -> String {
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

#[cfg(test)]
fn conversation_title_request(kind: AgentKind, text: &str) -> Option<ConversationTitleRequest> {
    build_title_request(kind, text, tab_title_from_prompt)
}

impl AgentPane {
    #[cfg(test)]
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
    #[cfg(test)]
    pub fn new_resuming(
        profile: AgentProfile,
        workspace: AgentWorkspace,
        resume: Option<RecoveryIdentity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = AgentSession::create(profile, workspace, cx);
        let mut pane = Self::attach(&owner, window, cx);

        pane.owned_session = Some(owner);
        pane.start_session_with_options(resume, false, |_, _, _| {}, cx);

        pane
    }

    pub(in crate::agent_tab) fn attach_team_member(
        owner: &SessionOwner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self::attach(owner, window, cx);

        pane.team_member = true;

        pane.transcript
            .update(cx, |transcript, _| transcript.clear_owner());

        pane
    }

    pub fn attach(owner: &SessionOwner, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = owner.session();
        let profile = host.read(cx).profile.clone();
        let workspace = host.read(cx).workspace.clone();
        let session = host.read(cx).controller.clone();
        let route = host.read(cx).route.clone();
        let binding = owner.bind();
        let kind = AgentKind::from_profile(profile.kind);
        let cwd = workspace.primary().map(str::to_string);
        let input_history_scope = InputHistoryScope::local(kind, &workspace);
        let name = kind.display();

        // Auto-grow wraps long prompts instead of scrolling them off-screen.
        // The view intercepts modified Enter actions before this input's
        // submit-on-enter behavior runs.
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(t!("agent-session-message-placeholder", name = name).into_owned())
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
            } else if let InputEvent::ClickLink(range) = event {
                this.open_attached_image(range.clone(), window, cx);
            }
        })
        .detach();

        // The rows that branch or rewind the conversation address the pane, so
        // the pane's own transcript is told which pane it belongs to; a view
        // mirroring somebody else's conversation is left without one.
        let owner = cx.entity().downgrade();

        let transcript = cx.new(|cx| {
            let mut transcript = TranscriptView::new(kind, cwd.clone());

            transcript.attach_content(session.borrow().conversation.clone(), cx);
            transcript.set_owner(owner);

            transcript
        });

        let mut this = Self {
            focus: cx.focus_handle(),
            agent_route: route,
            kind,
            profile,
            // Nothing has started yet, so the active snapshot matches the
            // configured list until the first conversation clones it.
            active_workspace: workspace.clone(),
            workspace,
            input_history_scope,
            input_history_navigation: InputHistoryNavigation::default(),
            attachments: ComposerAttachments::default(),
            transcript,
            input,
            session,
            host: host.downgrade(),
            binding,
            presenting_session_effect: false,
            team_member: false,
            #[cfg(test)]
            owned_session: None,
            history_ui: SessionHistoryUi::default(),
            prompts: PendingPrompts::default(),
            controls: ThreadControls { effort_drag: None },
            turn: TurnPresentation::default(),
            palette: SlashPalette {
                provider_commands_ready: !kind.caps().async_command_discovery,
                ..SlashPalette::default()
            },
            branch: BranchFlow::default(),
            git_branch_poll: GitBranchPoll::default(),
            session_state: SessionStateBadge,
            workflows: WorkflowUi::default(),
            overlay_fade: Fade::default(),
        };

        {
            let state = this.session.borrow();

            for index in 0..state.input.batches().len() {
                this.prompts.reveal(&state.input, index);
            }

            this.prompts.hide_settled(&state.input);

            if let Some(commands) = &state.command_catalog {
                this.palette.provider_commands = commands.clone();
                this.palette.provider_commands_ready = true;
            }

            this.palette.skill_catalog = state.skill_catalog.clone();
        }

        this.active_workspace = host.read(cx).active_workspace.clone();
        this.turn.refresh_timer(cx);

        if this.transcript.read(cx).is_working() {
            this.start_working(cx);
        }

        cx.observe(host, |this, _, cx| {
            this.transcript.update(cx, |view, cx| {
                view.sync_content();

                cx.notify();
            });

            cx.notify();
        })
        .detach();

        cx.subscribe(host, |this, _, event: &PresentationEffect, cx| {
            if this.binding.is_current()
                && this.binding.generation == event.generation
                && this.session.borrow().runtime.is_current(event.epoch)
                && let Some(effect) = event.effect.borrow_mut().take()
            {
                this.present_session_effect(effect, cx);
            }
        })
        .detach();

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
    pub(in crate::agent_tab) fn cwd(&self) -> Option<String> {
        self.workspace.primary().map(str::to_string)
    }

    /// The directories this tab is currently configured with. A conversation
    /// started from now on receives these.
    pub(in crate::agent_tab) fn configured_workspace(&self) -> &AgentWorkspace {
        &self.workspace
    }

    /// The directories the running conversation was started with.
    #[cfg(test)]
    pub(in crate::agent_tab) fn active_workspace(&self) -> &AgentWorkspace {
        &self.active_workspace
    }

    /// Replace the configured directory list after the parent workspace was
    /// edited. The running conversation keeps the snapshot it started with;
    /// the next one clones this.
    pub fn set_workspace(&mut self, workspace: AgentWorkspace, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if self.workspace == workspace {
            return;
        }

        let primary_changed = self.workspace.primary() != workspace.primary();

        self.input_history_scope = InputHistoryScope::local(self.kind, &workspace);

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, _| host.workspace = workspace.clone());
        }

        self.workspace = workspace;

        if primary_changed {
            self.git_branch_poll.invalidate();
            self.refresh_git_branch(cx);
        }

        cx.notify();
    }

    /// Append one item to the conversation, tagged with the current turn so
    /// settled turns fold as one unit.
    pub(in crate::agent_tab) fn push_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        self.push_item_with_images(item, Vec::new(), cx);
    }

    /// Append an item along with the images it carried, which only a sent
    /// user message has.
    pub(in crate::agent_tab) fn push_item_with_images(
        &mut self,
        item: SessionItem,
        images: Vec<Arc<Image>>,
        cx: &mut Context<Self>,
    ) {
        let turn = self.session.borrow().delivery.turn();

        self.transcript
            .update(cx, |transcript, cx| transcript.push(turn, item, images, cx));

        cx.notify();
    }

    pub(super) fn refresh_git_branch(&mut self, cx: &mut Context<Self>) {
        let Some(generation) = self.git_branch_poll.begin_refresh() else {
            return;
        };

        let Some(cwd) = self.cwd().or_else(|| {
            env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        }) else {
            self.git_branch_poll.complete(generation, None);

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
                this.git_branch_poll.complete(generation, branch);

                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn agent_session(&self) -> Option<Entity<AgentSession>> {
        self.host.upgrade()
    }

    pub(super) fn emit_event(&self, event: AgentPaneEvent, cx: &mut Context<Self>) {
        if self.presenting_session_effect {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |_, cx| cx.emit(event.clone()));
        }

        #[cfg(test)]
        cx.emit(event);
    }

    pub(super) fn emit_lifecycle(
        &self,
        kind: AgentEventKind,
        title: &str,
        body: &str,
        cx: &mut Context<Self>,
    ) {
        if self.presenting_session_effect {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.emit_lifecycle(kind, title, body, cx));
        }
    }

    pub(super) fn latest_agent_message(&self, cx: &App) -> Option<String> {
        self.transcript
            .read(cx)
            .latest_agent_message(self.session.borrow().delivery.turn())
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
        if !self.binding.is_current() {
            return false;
        }

        let title_text = restore_on_interrupt
            .as_ref()
            .map_or(text.as_str(), |(prompt, _)| prompt.as_str());

        let title_request =
            self.session
                .borrow()
                .naming
                .request(self.kind, title_text, tab_title_from_prompt);

        let settings = self.session.borrow().controls.settings.clone();
        let scratch = scratch_dir(self.agent_route.as_str());

        let restores_annotations = restore_on_interrupt.is_some();

        let image_prompt = (!self.attachments.images().is_empty()).then(|| text.clone());

        let outcome = self.session.borrow_mut().submit(
            text,
            |session, text| {
                let images = self
                    .attachments
                    .images()
                    .iter()
                    .map(|image| ImageAttachment {
                        bytes: image.bytes(),
                        media_type: image.format().mime_type(),
                    });

                match title_request.as_ref() {
                    Some(title) => session.send_user_message_with_title(
                        text, &settings, skill, images, &scratch, title,
                    ),

                    None => session.send_user_message(text, &settings, skill, images, &scratch),
                }
            },
            || {
                restore_on_interrupt.map(|(text, response_annotations)| RecoverablePrompt {
                    text,
                    response_annotations,
                    skill: skill.cloned(),
                })
            },
        );

        let started_text = match outcome {
            Ok(Submission::Started { text }) => Some(text),
            Ok(Submission::Queued) => None,

            Ok(Submission::NotReady) => {
                self.push_item(
                    SessionItem::Error {
                        text: t!("agent-session-still-starting", name = self.kind.display())
                            .into_owned(),
                    },
                    cx,
                );

                return false;
            }

            Ok(Submission::Rejected { message }) => {
                self.push_item(SessionItem::Error { text: message }, cx);

                return false;
            }

            Err(reason) => {
                let (kind, message) = match reason {
                    SubmissionBlock::QuestionResponse => {
                        (CommandFeedbackKind::Notice, "agent-question-send-pending")
                    }

                    SubmissionBlock::ConversationChange => (
                        CommandFeedbackKind::Error,
                        "agent-session-rewind-blocks-send",
                    ),

                    SubmissionBlock::CommandStarting => {
                        (CommandFeedbackKind::Error, "agent-session-command-starting")
                    }
                };

                self.palette.set_feedback(kind, t!(message), cx);

                return false;
            }
        };

        // Both providers generate their final title asynchronously. Claiming
        // the first accepted prompt here prevents a failed generation from
        // naming the conversation from a later message.
        if matches!(self.kind, AgentKind::Codex | AgentKind::Claude)
            && let Some(title) = title_request
        {
            self.session.borrow_mut().naming.named = true;

            self.emit_event(AgentPaneEvent::TitleSuggested(title.provisional_title), cx);
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

        if restores_annotations {
            self.attachments.clear_annotations();
        }

        // The first message commits this tab to its conversation; the
        // history list is no longer offered.
        self.history_ui.mode = RecentSessionsMode::Hidden;

        match started_text {
            Some(text) => {
                let _ = text;
                let shared = self.session.borrow().conversation.clone();
                let mut conversation = shared.borrow_mut();

                conversation.attach_last_images(
                    sent_images
                        .into_iter()
                        .map(|image| Arc::new(ConversationImage::new(image.bytes().into())))
                        .collect(),
                );

                drop(conversation);

                self.transcript
                    .update(cx, |transcript, _| transcript.sync_content());

                self.start_working(cx);
            }

            None => {
                if let Some(text) = image_prompt {
                    let images = sent_images
                        .into_iter()
                        .map(|image| Arc::new(ConversationImage::new(image.bytes().into())))
                        .collect();

                    self.session
                        .borrow_mut()
                        .pending_images
                        .push_back((text, images));
                }

                cx.notify();
            }
        }

        true
    }

    pub(super) fn clear_conversation_presentation(&mut self, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |transcript, _| transcript.reset_presentation());

        self.branch.clear();
        self.history_ui.invalidate_filesystem_history();

        // Workflow runs are scoped the same way, and their refresh must not
        // keep polling a directory that belongs to the replaced conversation.
        self.clear_workflows();

        // The question card is answered into the backend being replaced, so it
        // cannot outlive it either.
        self.prompts.clear();
    }

    /// Pass a tab rename through to the conversation, so the name reaches the
    /// harness's own session record rather than living only in this tab.
    pub fn rename_session(&mut self, title: &str) {
        if !self.binding.is_current() {
            return;
        }

        self.session.borrow_mut().naming.rename(title);
        self.sync_pending_rename();
    }

    pub(super) fn sync_pending_rename(&mut self) {
        if !self.binding.is_current() {
            return;
        }

        {
            let mut guard = self.session.borrow_mut();
            let state = &mut *guard;

            state.naming.sync(state.runtime.backend_mut())
        };
    }

    pub(super) fn reset_conversation(&mut self, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.reset(cx));
        }

        self.clear_conversation_presentation(cx);
        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;

        self.palette
            .reset_discovery(!self.kind.caps().async_command_discovery);

        self.session.borrow_mut().commands.clear();
        self.palette.feedback = None;
        self.history_ui.mode = RecentSessionsMode::Hidden;

        cx.notify();
    }
}
