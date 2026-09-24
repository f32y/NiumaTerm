//! The agent conversation pane: one backend session per tab, the composer,
//! the transcript, thread controls, and child-agent views. Harnesses may share
//! a process while keeping their session state isolated.
//!
//! The application shell owns tabs, chrome, provider updates, and settings;
//! this module reads only the [`settings::AgentSettings`] snapshot the shell
//! installs and exposes the pane plus the recovery types the update
//! coordinator drives across a backend replacement.

pub use crate::agent_tab::profile::{
    AgentKind, AgentKindExt, agent_launch, saved_settings_from_thread, thread_settings_from_saved,
};
pub use crate::agent_tab::session::{
    RecoveryIdentity, RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};

pub mod execution;
pub mod input_history;
pub mod profile;
pub mod settings;
pub mod team;
pub mod transcript;

mod capabilities;
mod commands;
mod composer;
mod context_usage;
mod fade;
mod pane_state;
mod questions;
mod session;
mod thread_controls;
mod view;
mod workflows;

#[cfg(test)]
mod tests;

use std::cell::{Ref, RefCell};
use std::env;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, Image, IntoElement, MouseButton,
    MouseUpEvent, Pixels, Render, SharedString, WeakEntity, Window, div, px, relative, size,
};
use gpui_base::TextSelection;
use gpui_component::input::{
    Enter, Escape, IndentInline, InputEvent, MoveDown, MoveUp, Paste, Textarea, TextareaState,
};
use gpui_component::{ActiveTheme as _, ElementExt as _, WindowExt, v_flex};
use nmt_agent::background_task::{BackgroundTaskKey, BackgroundTaskSnapshot};
use nmt_agent::catalog::adapter_commands;
use nmt_agent::chat::{
    ForkCheckpoint, Item as SessionItem, Question, SendOutcome, SessionSummary, SkillInfo,
    SkillReference, SlashCommandArguments, SlashCommandInfo, SlashCommandOutcome,
    SlashCommandRunPolicy, SlashCommandSource,
};
use nmt_agent::claude_code::stream_json;
use nmt_agent::codex::app_server;
use nmt_agent::session::branch::{
    BranchCompletion, BranchError, BranchFailure, BranchUpdate, BranchView, FileProgress,
    PromptTarget,
};
use nmt_agent::session::children::ChildTranscript;
use nmt_agent::session::commands::CommandAdmission;
use nmt_agent::session::controller::{
    SessionController, SessionEffect, SessionFailure, SessionReady, SubmissionBlock,
};
use nmt_agent::session::delivery::RecoverablePrompt;
use nmt_agent::session::input::{
    ApprovalOutcome, QuestionAction, QuestionCompletion, QuestionKey, Submission,
};
use nmt_agent::session::lifecycle::InterruptOutcome;
use nmt_agent::session::restore::{ResumeStart, SettingsSeed};
use nmt_agent::session::side::SideQuestionOutcome;
use nmt_agent::session::workflows::OpenWorkflowAgent;
use nmt_agent::session::{ImageAttachment, PromptRequest, SettingsOutcome};
#[cfg(test)]
use nmt_agent::transcript::TextField;
use nmt_agent::transcript::conversation::ConversationImage;
use nmt_agent::workflow::WorkflowRun;
use nmt_agent::{AgentEvent, AgentEventKind, AgentRoute, AgentWorkspace};
use nmt_config::profile::AgentProfile;
use rust_i18n::t;
use tracing::info;

use crate::agent_tab::capabilities::AgentCapabilities as _;
use crate::agent_tab::commands::{
    PaletteCatalogEntry, PaletteDirection, SlashRoute, filter_palette_catalog,
    filter_skill_catalog, local_commands, merge_catalog, move_palette_selection,
    parse_skill_prefix, parse_slash_command, prepare_skill_selection, reconcile_skill_binding,
    route_slash, setting_value_label, status_summary, validate_skill_binding,
};
use crate::agent_tab::composer::attachments::{
    ComposerAttachments, MAX_ATTACHMENTS, THUMBNAIL, has_image, prepare_paste, scratch_dir,
};
use crate::agent_tab::composer::{
    BranchFlow, CachedCatalog, CommandFeedbackKind, PaletteAction, PaletteModel, PaletteRow,
    PendingSlashCommand, RewindAction, SlashPalette, branch_error_message, branch_failure_message,
    fork_palette_model, prompt_with_response_annotations, restored_input_after_interruption,
    rewind_palette_model, row_prompt_target,
};
use crate::agent_tab::execution::{
    AgentSession, ChildReader, CommandBinding, PresentationEffect, SessionOwner,
};
use crate::agent_tab::fade::FrostedLayer;
use crate::agent_tab::input_history::{
    InputHistoryAction, InputHistoryDirection, InputHistoryNavigation, InputHistoryScope,
};
use crate::agent_tab::pane_state::TurnPresentation;
use crate::agent_tab::questions::panel::QuestionPanel;
use crate::agent_tab::session::errors::operation_error;
use crate::agent_tab::session::{Backend, Status};
use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::thread_controls::{launch_model, remember_defaults, render_row};
use crate::agent_tab::transcript::{TranscriptView, last_response_label, transcript_column};
use crate::agent_tab::view::approval_card::approval_card;
use crate::agent_tab::view::blocking_overlay::{
    BlockingOverlay, start_overlay, update_banner, update_overlay,
};
use crate::agent_tab::view::cache_expiry::cache_expiry_dialog;
use crate::agent_tab::view::composer_layout::{
    ComposerEnterBehavior, composer_card, composer_controls_row, composer_enter_behavior,
    composer_input_row, composer_notice_panel, send_button,
};
use crate::agent_tab::view::composer_notices::{
    last_response_mark, multi_root_notice, multi_root_strip, queued_prompts,
};
use crate::agent_tab::view::composer_status::{ComposerStatusBar, poll_git_branch};
use crate::agent_tab::view::progress_panel::ProgressPanel;
use crate::agent_tab::view::recent_sessions::{ListControl, RecentSessionsMode, SessionHistoryUi};
use crate::agent_tab::view::selection_menu::show_selected_text_menu;
use crate::agent_tab::view::side_questions::{SideChatWindow, side_chat_window};
use crate::agent_tab::workflows::WorkflowUi;

#[derive(Clone)]
pub enum AgentPaneEvent {
    Lifecycle(AgentEvent),
    Interrupted,
    /// This tab's workflow picture changed: it gained its first run, or its
    /// count of running agents moved. Reported as an event so the chrome can
    /// track it without observing every pane repaint.
    WorkflowActivity,
    /// This tab's count of running child agents moved. Reported as an event so
    /// the chrome can track it without observing every pane repaint.
    BackgroundTaskActivity,
    /// A conversation this pane listed but cannot continue: it ran in another
    /// directory, and a tab is rooted in the one it was opened for. The chrome
    /// owns tabs, so opening it where it worked is left to the chrome.
    ResumeElsewhere {
        cwd: String,
        summary: SessionSummary,
    },
    /// A name for the conversation this pane is holding, derived from the
    /// message that opened it. The pane does not know which tab owns it, so
    /// naming the tab is left to the chrome that does. An empty name means the
    /// pane no longer holds a conversation worth naming, which drops the tab
    /// back to the name its profile gives it.
    TitleSuggested(String),
    /// The tab holding this pane should close. A pane owns no tab, so the
    /// chrome that does is asked to close it.
    CloseRequested,
    /// Text the user wrote to a Team member in the member's own view. The
    /// member's requests come from its room, so the Team that owns the room
    /// sends it and records the exchange where every member can see it.
    TeamPrompt(String),
    /// This tab's side chat appeared, went away, or was minimized or
    /// restored. The chrome's Side Chat control follows it.
    SideChatActivity,
}

pub struct AgentPane {
    focus: FocusHandle,
    input_history_scope: InputHistoryScope,
    input_history_navigation: InputHistoryNavigation,

    /// Images the pending message carries, anchored to the composer text by
    /// their `[Image #N]` placeholders, and the response text quoted into it.
    attachments: ComposerAttachments,

    /// The conversation as the user reads it. Presentation lives in its own
    /// view so a child agent's conversation renders through the same code.
    transcript: Entity<TranscriptView>,

    input: Entity<TextareaState>,
    history_ui: SessionHistoryUi,
    progress_panel: ProgressPanel,
    side_chat: SideChatWindow,

    /// Provider state and transitions, independent of widgets and rendering.
    session: Rc<RefCell<SessionController>>,

    host: WeakEntity<AgentSession>,
    binding: CommandBinding,
    team_member: bool,
    #[cfg(test)]
    owned_session: Option<SessionOwner>,

    /// Interaction state for the thread controls under the composer.
    effort_drag: Option<usize>,

    /// The running turn's bookkeeping, from submission to settled output.
    turn: TurnPresentation,

    /// The approval and question cards that block a turn until answered.
    prompts: QuestionPanel,

    palette: SlashPalette,

    /// Cutting the conversation at an earlier point, by rewind or by fork.
    branch: BranchFlow,

    composer_status: ComposerStatusBar,

    /// Workflow runs of this session and the agent conversation the user has
    /// open. Workflow agents are not child agents, so they never reach the
    /// `Background Tasks` state above.
    workflows: WorkflowUi,

    /// Ramp of the layer that covers the pane while its backend cannot take
    /// input. Cross-fading the whole layer keeps its arrival readable as the
    /// tab being held rather than as a blur being switched on.
    blocking_overlay: BlockingOverlay,
}

impl AgentPane {
    /// Whether such a flow is past its picker and working. Until then the
    /// input still holds text worth editing, so only sending is refused.
    pub(crate) fn branch_flow_is_working(&self) -> bool {
        self.session.borrow().branch().is_working()
    }

    /// Whether a list of branch points is on screen, which is what makes the
    /// palette's highlight something the transcript follows.
    pub(crate) fn branch_picker_is_open(&self) -> bool {
        self.session.borrow().branch().picker_is_open()
    }

    /// Hand the transcript to a picker that is about to scroll it to the
    /// prompt it highlights.
    pub(crate) fn hold_transcript_for_picker(&self, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |transcript, _| transcript.hold_for_picker());
    }

    /// Give it back, for a picker closing without having cut anything.
    pub(crate) fn release_transcript_from_picker(&self, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |transcript, cx| transcript.release_from_picker(cx));
    }

    /// Move the transcript to the prompt the highlighted picker row names, so
    /// the conversation shows what the cut would keep and what it would drop.
    /// Following the smooth_wheel-scrolling setting keeps the jump between two
    /// distant prompts readable where the user asked for animated scrolling.
    pub(crate) fn follow_branch_selection(&mut self, cx: &mut Context<Self>) {
        let selected = self.palette.selected;

        let Some(target) = self
            .palette_model(cx)
            .and_then(|model| model.rows.get(selected).cloned())
            .and_then(|row| row_prompt_target(selected, &row.action))
        else {
            return;
        };

        let smooth_wheel = cx.global::<AgentSettings>().smooth_wheel;

        self.transcript.update(cx, |transcript, cx| {
            transcript.scroll_to_prompt(&target, smooth_wheel, cx)
        });
    }

    pub(crate) fn cancel_branch_picker(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        if !self.session.borrow_mut().cancel_branch_picker() {
            return false;
        }

        self.branch.draft = None;
        self.palette.selected = 0;
        self.palette.feedback = None;

        self.release_transcript_from_picker(cx);

        cx.notify();

        true
    }

    pub(crate) fn fork_from_prompt(
        &mut self,
        target: PromptTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        self.request_fork_checkpoints(Some(target), cx)
    }

    pub(crate) fn open_fork(&mut self, cx: &mut Context<Self>) -> bool {
        self.request_fork_checkpoints(None, cx)
    }

    fn request_fork_checkpoints(
        &mut self,
        target: Option<PromptTarget>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        if self.session.borrow().runtime().status() != Status::Idle || self.is_command_busy() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                SharedString::from(t!("agent-fork-idle-only")),
                cx,
            );

            return false;
        }

        if let Err(error) = self.session.borrow_mut().begin_fork(target) {
            let message = match error {
                BranchError::Busy => SharedString::from(t!("agent-fork-idle-only")),
                _ => self.branch_error_message(error, cx).into(),
            };

            self.palette
                .set_feedback(CommandFeedbackKind::Error, message, cx);

            return false;
        }

        self.branch.reset_pending_prompt();

        self.palette.selected = 0;
        self.palette.dismissed = false;

        self.palette.set_feedback(
            CommandFeedbackKind::Status,
            SharedString::from(t!("agent-fork-loading-checkpoints")),
            cx,
        );

        true
    }

    pub(crate) fn on_fork_update(&mut self, update: BranchUpdate, cx: &mut Context<Self>) {
        match update {
            BranchUpdate::Empty => self.palette.set_feedback(
                CommandFeedbackKind::Error,
                SharedString::from(t!("agent-fork-no-prompts")),
                cx,
            ),
            BranchUpdate::Picker { unresolved } => {
                self.palette.feedback = None;
                self.palette.selected = 0;

                if unresolved {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        SharedString::from(t!("agent-fork-prompt-not-a-branch-point")),
                        cx,
                    );
                }

                self.hold_transcript_for_picker(cx);

                self.follow_branch_selection(cx);

                cx.notify();
            }
            BranchUpdate::Branching => {
                self.branch.draft = Some(self.input.read(cx).text().to_string());
                self.history_ui.mode = RecentSessionsMode::Loading;

                self.session.borrow_mut().begin_branched_conversation();

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    SharedString::from(t!("agent-session-forking")),
                    cx,
                );

                cx.notify();
            }
            BranchUpdate::Failed(failure) => {
                let message = self.branch_error_message(failure.error, cx);

                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
            }
            _ => {}
        }
    }

    pub(crate) fn cancel_fork_picker(&mut self, cx: &mut Context<Self>) {
        self.cancel_branch_picker(cx);
    }

    pub(crate) fn start_conversation_branch(
        &mut self,
        checkpoint: ForkCheckpoint,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        let update = self.session.borrow_mut().fork(checkpoint);

        self.on_fork_update(update, cx);
    }

    pub(crate) fn branch_flow_holds_composer(&self) -> bool {
        self.session.borrow().branch().holds_composer()
    }

    pub(crate) fn complete_branch(&mut self, completion: BranchCompletion, cx: &mut Context<Self>) {
        let message = match (completion.replayed, completion.files) {
            (_, FileProgress::Restored) => "agent-rewind-complete-with-files",
            (true, FileProgress::NotConfirmed) => "agent-rewind-complete",
            (false, FileProgress::NotConfirmed) => "agent-fork-complete",
        };

        let draft = self.branch.draft.take();

        self.clear_conversation_presentation(cx);

        self.history_ui.mode = RecentSessionsMode::Hidden;

        self.branch.prepare_prompt(draft, completion.prompt);

        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            SharedString::from(t!(message)),
            cx,
        );

        cx.notify();
    }

    pub(crate) fn rewind_to_prompt(
        &mut self,
        target: PromptTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        self.load_rewind_checkpoints(Some(target), cx)
    }

    pub(crate) fn open_rewind(&mut self, cx: &mut Context<Self>) -> bool {
        self.load_rewind_checkpoints(None, cx)
    }

    fn load_rewind_checkpoints(
        &mut self,
        target: Option<PromptTarget>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        if self.session.borrow().runtime().status() != Status::Idle || self.is_command_busy() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                SharedString::from(t!("agent-rewind-idle-only")),
                cx,
            );

            return false;
        }

        let cwd = self.cwd(cx);

        let outcome = self.session.borrow_mut().begin_rewind(cwd, target);

        let request = match outcome {
            Ok(request) => request,
            Err(error) => {
                let message = self.branch_error_message(error, cx);

                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);

                return false;
            }
        };

        self.branch.reset_pending_prompt();

        self.palette.selected = 0;
        self.palette.dismissed = false;

        self.palette.set_feedback(
            CommandFeedbackKind::Status,
            SharedString::from(t!("agent-rewind-loading-checkpoints")),
            cx,
        );

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.read_checkpoints(request, cx));
        }

        true
    }

    pub(crate) fn cancel_rewind_picker(&mut self, cx: &mut Context<Self>) {
        self.cancel_branch_picker(cx);
    }

    pub(crate) fn activate_rewind_action(&mut self, action: RewindAction, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if action == RewindAction::Cancel {
            self.cancel_rewind_picker(cx);

            return;
        }

        self.branch.draft = Some(self.input.read(cx).text().to_string());

        let update = self.session.borrow_mut().rewind(action);

        self.on_rewind_update(update, cx);
    }

    pub(crate) fn on_rewind_update(&mut self, update: BranchUpdate, cx: &mut Context<Self>) {
        match update {
            BranchUpdate::Ignored => {}
            BranchUpdate::Empty => self.palette.set_feedback(
                CommandFeedbackKind::Error,
                SharedString::from(t!("agent-rewind-no-prompts")),
                cx,
            ),
            BranchUpdate::Picker { unresolved } => {
                self.palette.selected = 0;
                self.palette.feedback = None;

                if unresolved {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        SharedString::from(t!("agent-rewind-prompt-not-a-checkpoint")),
                        cx,
                    );
                }

                self.hold_transcript_for_picker(cx);

                self.follow_branch_selection(cx);

                cx.notify();
            }
            BranchUpdate::RestoringFiles(action) => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Status,
                    match action {
                        RewindAction::FilesAndConversation => {
                            SharedString::from(t!("agent-rewind-restoring-before-fork"))
                        }
                        _ => SharedString::from(t!("agent-rewind-restoring-files")),
                    },
                    cx,
                );
            }
            update @ (BranchUpdate::CreateFork(_) | BranchUpdate::StartSession(_)) => {
                self.history_ui.mode = RecentSessionsMode::Loading;

                self.palette.reset_discovery();

                if let Some(host) = self.host.upgrade() {
                    host.update(cx, |host, cx| host.on_branch_update(update, cx));
                }
            }
            BranchUpdate::FilesRestored => {
                self.branch.draft = None;

                self.release_transcript_from_picker(cx);

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    SharedString::from(t!("agent-rewind-files-restored")),
                    cx,
                );
            }
            BranchUpdate::Failed(failure) => self.report_branch_failure(failure, cx),
            BranchUpdate::Branching => {}
        }
    }

    pub(crate) fn branch_error_message(&self, error: BranchError, cx: &App) -> String {
        self.host
            .upgrade()
            .map(|host| branch_error_message(error, host.read(cx).kind))
            .unwrap_or_default()
    }

    pub(crate) fn report_branch_failure(&mut self, failure: BranchFailure, cx: &mut Context<Self>) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let message = branch_failure_message(failure, session_host.read(cx).kind);

        self.palette.selected = 0;

        self.palette
            .set_feedback(CommandFeedbackKind::Error, message, cx);
    }

    /// Take a pasted image into the pending message, reporting whether the
    /// paste was consumed. A paste this leaves alone falls through to the
    /// composer's own text handling, which is what a clipboard holding text
    /// should get.
    pub(crate) fn paste_image(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(host) = self.host.upgrade() else {
            return false;
        };

        // An image reaches the clipboard two ways: as pixels, from a capture
        // tool or a browser, and as a file, from a file manager. Both are the
        // same gesture to the person doing it.
        let entries: Vec<_> = cx
            .read_from_clipboard()
            .into_iter()
            .flat_map(|item| item.into_entries())
            .collect();

        if !has_image(&entries) {
            return false;
        }

        if self.attachments.images().iter().count() + self.attachments.preparing >= MAX_ATTACHMENTS
        {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-composer-images-full", count = MAX_ATTACHMENTS).into_owned(),
                cx,
            );

            return true;
        }

        let host = host.read(cx);
        let scratch = (host.kind == AgentKind::Codex).then(|| scratch_dir(host.route.as_str()));
        let epoch = self.session.borrow().runtime().epoch();
        let binding_generation = self.binding.generation;
        let executor = cx.background_executor().clone();

        self.attachments.preparing += 1;

        let previous = self.attachments.paste_ready.take();
        let (completed, ready) = oneshot::channel();

        self.attachments.paste_ready = Some(ready);

        let prepared = executor.clone().spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }

            prepare_paste(entries, scratch, executor)
        });

        cx.spawn_in(window, async move |this, cx| {
            let prepared = prepared.await;

            let _ = this.update_in(cx, |this, window, cx| {
                this.attachments.preparing = this.attachments.preparing.saturating_sub(1);

                cx.notify();

                if !this.binding.is_current()
                    || this.binding.generation != binding_generation
                    || this.session.borrow().runtime().epoch() != epoch
                {
                    return;
                }

                match prepared {
                    Ok(mut prepared) => {
                        let path = prepared.path.clone();

                        if this
                            .attachments
                            .attach_prepared(prepared.image.clone(), path, &this.input, window, cx)
                            .is_ok()
                        {
                            prepared.path = None;
                        }
                    }
                    Err(message) => {
                        this.palette
                            .set_feedback(CommandFeedbackKind::Error, message, cx)
                    }
                }
            });

            let _ = completed.send(());
        })
        .detach();

        cx.notify();

        true
    }

    pub(crate) fn remove_attachment(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .attachments
            .remove_image(index, &self.input, window, cx)
        {
            cx.notify();
        }
    }

    pub(crate) fn sync_attachments(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.attachments.sync(text, &self.input, window, cx) {
            cx.notify();
        }
    }

    /// Open a pending image over the message stream, in the same layer a sent
    /// image opens in: an attachment is read at full size the same way
    /// wherever the reader meets it. `origin` is the thumbnail it was opened
    /// from, when it was opened from one, for the preview to grow out of.
    pub(crate) fn open_image(
        &mut self,
        image: Arc<Image>,
        origin: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        self.transcript.update(cx, |transcript, cx| {
            transcript.zoom_image(image, origin, cx)
        });
    }

    /// Open the image a composer placeholder names. The placeholder is the
    /// only thing in the pending message that stands for an image, so
    /// following it shows what it stands for.
    pub(crate) fn open_attached_image(
        &mut self,
        range: Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.input.read(cx).text().to_string();

        let Some(image) = self
            .attachments
            .images()
            .linked_image(&text, range)
            .cloned()
        else {
            return;
        };

        // The link is a run of text with no picture in it, so the preview
        // grows out of a thumbnail's worth of space under the pointer: what
        // was clicked is where the image comes from.
        let origin =
            Bounds::centered_at(window.mouse_position(), size(px(THUMBNAIL), px(THUMBNAIL)));

        self.open_image(image, Some(origin), cx);
    }

    /// Send what the composer holds, warning first when the conversation has
    /// been idle long enough for the provider's prompt cache to have expired.
    pub(super) fn send_user_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.team_member {
            let text = self.input.read(cx).text().to_string();

            if !text.trim().is_empty() {
                cx.emit(AgentPaneEvent::TeamPrompt(text));
            }

            return;
        }

        // Slash lines steer the session (`/new`, `/model`, `/status`) rather
        // than continue the conversation, so a warning about what the next
        // answer costs would fire in front of commands that ask for none.
        let text = self.input.read(cx).text().to_string();

        if !text.trim().is_empty()
            && parse_slash_command(&text).is_none()
            && self.prompt_cache_may_have_expired(cx)
        {
            self.confirm_send_after_cache_expiry(window, cx);

            return;
        }

        self.send_user_message_now(window, cx);
    }

    /// Whether the idle span since the agent last answered has passed the
    /// profile's warning threshold. A running turn is still writing into the
    /// live cache, so a mid-turn steer never counts as a cold start.
    fn prompt_cache_may_have_expired(&self, cx: &Context<Self>) -> bool {
        let Some(session_host) = self.host.upgrade() else {
            return false;
        };

        let session_profile = session_host.read(cx).profile.clone();

        let minutes: u64 = session_profile.cache_warn_minutes.into();

        minutes > 0
            && !self.transcript.read(cx).is_working()
            && self
                .session
                .borrow()
                .conversation()
                .borrow()
                .last_response_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(minutes * 60))
    }

    /// Ask before paying for a cold prompt cache. Cancelling leaves the text
    /// in the composer, so the decision costs nothing to reverse.
    fn confirm_send_after_cache_expiry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let idle = self
            .session
            .borrow()
            .conversation()
            .borrow()
            .last_response_at
            .map(|at| last_response_label(at.elapsed().as_secs()))
            .unwrap_or_default();

        let pane = cx.entity();

        window.open_dialog(cx, move |dialog, _, _| {
            cache_expiry_dialog(dialog, &pane, &idle)
        });
    }

    fn send_user_message_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

        if self.team_member {
            return;
        }

        if self.branch_flow_holds_composer() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-rewind-blocks-send").to_string(),
                cx,
            );

            return;
        }

        let text = self.input.read(cx).text().to_string();

        if parse_slash_command(&text).is_some() {
            self.submit_current_slash(window, cx);

            return;
        }

        let text = text.trim().to_string();

        if text.is_empty() {
            return;
        }

        reconcile_skill_binding(&text, &mut self.palette.skill_binding);

        let skill = if session_kind.caps().skill_references {
            match validate_skill_binding(
                &text,
                self.palette.skill_binding.as_ref(),
                self.session.borrow().skill_catalog(),
            ) {
                Ok(skill) => skill,
                Err(message) => {
                    self.palette
                        .set_feedback(CommandFeedbackKind::Error, message, cx);

                    return;
                }
            }
        } else {
            None
        };

        if self.send_text_with_skill(text.clone(), skill.as_ref(), cx) {
            self.input_history_navigation.record_input_history(
                &self.input_history_scope,
                &text,
                cx,
            );

            self.palette.skill_binding = None;

            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    pub(super) fn is_command_busy(&self) -> bool {
        self.attachments.preparing > 0
            || self.session.borrow().runtime().status() == Status::Running
            || self.session.borrow().commands().awaiting_turn
            || self.history_ui.mode == RecentSessionsMode::Loading
            || self.branch_flow_holds_composer()
    }

    pub(crate) fn open_recent_sessions(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_command_busy() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                SharedString::from(t!("agent-composer-resume-idle-only")),
                cx,
            );

            return false;
        }

        if !self.history_ui.open() {
            self.palette.set_feedback(
                CommandFeedbackKind::Notice,
                SharedString::from(t!("agent-composer-no-recent-sessions")),
                cx,
            );

            return true;
        }

        self.palette.feedback = None;

        cx.notify();

        true
    }

    pub(crate) fn palette_model(&mut self, cx: &Context<Self>) -> Option<PaletteModel> {
        let session_host = self.host.upgrade()?;

        let session_kind = session_host.read(cx).kind;

        match self.session.borrow().branch().into() {
            view @ (BranchView::LoadingRewind
            | BranchView::RewindCheckpoints(_)
            | BranchView::RewindAction(_, _)) => return rewind_palette_model(view),
            view @ (BranchView::LoadingFork | BranchView::ForkCheckpoints(_)) => {
                return fork_palette_model(view);
            }
            BranchView::Working => return None,
            BranchView::Idle => {}
        }

        if self.palette.dismissed {
            return None;
        }

        let (text, cursor) = {
            let input = self.input.read(cx);
            let document = input.text();

            // Reached from `render`, so this runs on every frame the pane
            // paints. Only a document opening with one of the two picker
            // sigils can produce a model, and its first character settles that
            // without walking the rope to copy the whole document out.
            if !matches!(document.chars().next(), Some('/' | '$')) {
                return None;
            }

            (document.to_string(), input.cursor())
        };

        if session_kind.caps().skill_references
            && let Some(query) = parse_skill_prefix(&text)
        {
            return Some(self.skill_palette_model(&query));
        }

        let parsed = parse_slash_command(&text)?;
        let catalog = self.command_catalog(cx);

        // A harness with `$name` skill references reaches them that way.
        // Listing them under `/` too is a convenience for users who expect one
        // command key, so it follows the compatibility setting as well. Where
        // `/name` is instead the only way to reach a skill, the listing is not
        // a convenience and follows nothing.
        let caps = session_kind.caps();

        let slash_skills = caps.slash_skills_are_prompts
            || (caps.skill_references && cx.global::<AgentSettings>().codex_skill_command_compat);

        if parsed.has_argument_separator {
            let command = catalog.iter().find(|command| command.name == parsed.name)?;

            if command.arguments == SlashCommandArguments::Skills {
                let query = parsed.arguments.trim().to_ascii_lowercase();

                return Some(self.skill_palette_model(&query));
            }

            if command.arguments != SlashCommandArguments::Choices {
                return None;
            }

            let query = parsed.arguments.to_ascii_lowercase();

            let rows = self
                .command_choices(&command.name, cx)
                .into_iter()
                .filter(|(value, label)| {
                    query.is_empty()
                        || value.to_ascii_lowercase().contains(&query)
                        || label.to_ascii_lowercase().contains(&query)
                })
                .map(|(value, label)| PaletteRow {
                    description: SharedString::new(&value),
                    label: label.into(),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::Choice {
                        command: command.name.clone(),
                        value,
                    },
                })
                .collect::<Vec<_>>();

            return Some(PaletteModel {
                note: rows
                    .is_empty()
                    .then(|| SharedString::from(t!("agent-composer-no-matching-values"))),
                rows,
            });
        }

        // Moving the caret into later prose must not turn an ordinary edit
        // into palette navigation; only the first slash token owns the keys.
        if cursor > 1 + parsed.name.len() {
            return None;
        }

        let session = self.session.borrow();

        let skills: &[SkillInfo] = if slash_skills {
            session
                .skill_catalog()
                .map(|catalog| catalog.skills.as_slice())
                .unwrap_or_default()
        } else {
            &[]
        };

        let rows = filter_palette_catalog(&catalog, skills, &parsed.name)
            .into_iter()
            .map(|entry| match entry {
                PaletteCatalogEntry::Command(command) => {
                    // A local command runs against the pane and stays available
                    // whatever the harness is doing; anything the harness owns
                    // needs a session that has finished starting and not ended.
                    let disabled_reason = if command.run_policy == SlashCommandRunPolicy::IdleOnly
                        && self.is_command_busy()
                    {
                        Some(SharedString::from(t!("agent-composer-available-when-idle")))
                    } else if command.source == SlashCommandSource::Local {
                        None
                    } else {
                        match self.session.borrow().runtime().status() {
                            Status::Starting => {
                                Some(SharedString::from(t!("agent-composer-agent-starting")))
                            }
                            Status::Exited => {
                                Some(SharedString::from(t!("agent-composer-agent-exited")))
                            }
                            _ => None,
                        }
                    };

                    PaletteRow {
                        label: format!("/{}", command.name).into(),
                        description: SharedString::new(&command.description),
                        hint: command.argument_hint.as_deref().map(SharedString::new),
                        disabled_reason,
                        action: PaletteAction::Command(command.clone()),
                    }
                }
                PaletteCatalogEntry::Skill(skill) => PaletteRow {
                    label: format!("/{}", skill.name).into(),
                    description: SharedString::new(&skill.description),
                    hint: Some(
                        t!("agent-composer-skill-scope", scope = &skill.scope)
                            .into_owned()
                            .into(),
                    ),
                    disabled_reason: self.skill_disabled_reason(skill),
                    action: PaletteAction::Skill(skill.clone()),
                },
            })
            .collect::<Vec<_>>();

        let note = if rows.is_empty() {
            if slash_skills && session.skill_catalog().is_none() {
                Some(SharedString::from(t!(
                    "agent-composer-skill-discovery-loading"
                )))
            } else if slash_skills
                && session
                    .skill_catalog()
                    .is_some_and(|catalog| !catalog.errors.is_empty())
            {
                session
                    .skill_catalog()
                    .and_then(|catalog| catalog.errors.first())
                    .map(SharedString::new)
            } else if slash_skills {
                Some(SharedString::from(t!(
                    "agent-composer-no-matching-commands-skills"
                )))
            } else {
                Some(SharedString::from(t!(
                    "agent-composer-no-matching-commands"
                )))
            }
        } else if session_kind.caps().async_command_discovery && session.command_catalog().is_none()
        {
            Some(SharedString::from(t!(
                "agent-composer-claude-command-loading"
            )))
        } else if slash_skills && session.skill_catalog().is_none() {
            Some(SharedString::from(t!(
                "agent-composer-skill-discovery-loading"
            )))
        } else if slash_skills {
            session
                .skill_catalog()
                .and_then(|catalog| catalog.errors.first())
                .map(|error| {
                    t!("agent-composer-skill-load-partial", error = error)
                        .into_owned()
                        .into()
                })
        } else {
            None
        };

        Some(PaletteModel { rows, note })
    }

    fn handle_palette_control(
        &mut self,
        control: PaletteControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.palette_model(cx) else {
            // The card is answered before the recent-sessions list or the input
            // history get a look, because the turn is blocked on it and neither
            // of those can lead anywhere until it is.
            if self.binding.is_current()
                && self
                    .prompts
                    .handle_control(control, self.session.borrow_mut().input_mut())
            {
                cx.stop_propagation();

                cx.notify();

                return;
            }

            let composer_empty = self.input.read(cx).text().len() == 0;
            let transcript_empty = self.transcript.read(cx).is_empty();

            match self
                .history_ui
                .handle_control(control, transcript_empty, composer_empty)
            {
                ListControl::Ignored => {}
                ListControl::Unchanged => {
                    cx.stop_propagation();

                    return;
                }
                ListControl::Changed => {
                    cx.stop_propagation();

                    cx.notify();

                    return;
                }
                ListControl::Resume(index) => {
                    cx.stop_propagation();

                    self.resume_session(index, cx);

                    return;
                }
            }

            let direction = match control {
                PaletteControl::Previous => Some(InputHistoryDirection::Older),
                PaletteControl::Next => Some(InputHistoryDirection::Newer),
                PaletteControl::Activate | PaletteControl::Complete | PaletteControl::Dismiss => {
                    None
                }
            };

            if direction
                .is_some_and(|direction| self.handle_input_history_control(direction, window, cx))
            {
                return;
            }

            cx.propagate();

            return;
        };

        cx.stop_propagation();

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                if let Some(direction) = control.direction()
                    && let Some(selected) =
                        move_palette_selection(self.palette.selected, model.rows.len(), direction)
                {
                    self.palette.selected = selected;

                    self.palette.scroll.scroll_to_item(self.palette.selected);

                    self.follow_branch_selection(cx);

                    cx.notify();
                }
            }
            PaletteControl::Activate => {
                if model.rows.is_empty() {
                    self.submit_current_slash(window, cx);
                } else {
                    self.activate_palette_index(self.palette.selected, true, window, cx);
                }
            }
            PaletteControl::Complete => {
                self.activate_palette_index(self.palette.selected, false, window, cx);
            }
            PaletteControl::Dismiss => {
                self.dismiss_command_palette(cx);
            }
        }
    }

    fn dismiss_command_palette(&mut self, cx: &mut Context<Self>) {
        if !self.cancel_branch_picker(cx) {
            self.palette.dismissed = true;

            cx.notify();
        }
    }

    /// Move the highlight to the row under the pointer without acting on it.
    ///
    /// Only a picker of branch points does this: there the highlight is what
    /// the transcript follows, so pointing at a prompt has to reach it the
    /// same way the arrow keys do. In the command palette the pointer often
    /// rests over the list while the user types, and moving the highlight
    /// there would change what Enter runs.
    fn hover_palette_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.palette.selected == index {
            return;
        }

        self.palette.selected = index;

        self.follow_branch_selection(cx);

        cx.notify();
    }

    pub(crate) fn activate_palette_index(
        &mut self,
        index: usize,
        execute: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

        if !self.binding.is_current() {
            return;
        }

        let Some(row) = self
            .palette_model(cx)
            .and_then(|model| model.rows.get(index).cloned())
        else {
            return;
        };

        if let Some(reason) = row.disabled_reason {
            self.palette
                .set_feedback(CommandFeedbackKind::Error, reason, cx);

            return;
        }

        let (text, can_execute) = match row.action {
            PaletteAction::Command(command) => {
                let needs_arguments = command.arguments != SlashCommandArguments::None;

                (
                    format!(
                        "/{}{}",
                        command.name,
                        if needs_arguments { " " } else { "" }
                    ),
                    !needs_arguments,
                )
            }
            PaletteAction::Choice { command, value } => (format!("/{command} {value}"), true),
            // Where a skill is written into the prompt, picking one lands the
            // token the harness will recognize and leaves the caret after it,
            // because what follows is the request the skill serves.
            PaletteAction::Skill(skill) if session_kind.caps().slash_skills_are_prompts => {
                let text = format!("/{} ", skill.name);

                self.input.update(cx, |input, cx| {
                    input.set_value(text.clone(), window, cx);

                    input.set_selected_range(text.len()..text.len(), cx);
                });

                self.palette.selected = 0;
                self.palette.dismissed = true;

                cx.notify();

                return;
            }
            PaletteAction::Skill(skill) => {
                let Ok((text, binding)) = prepare_skill_selection(&skill) else {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        t!("agent-command-skill-disabled-by-codex", name = &skill.name)
                            .into_owned(),
                        cx,
                    );

                    return;
                };

                self.input.update(cx, |input, cx| {
                    input.set_value(text.clone(), window, cx);

                    input.set_selected_range(text.len()..text.len(), cx);
                });

                self.palette.skill_binding = Some(binding);
                self.palette.selected = 0;
                self.palette.dismissed = true;

                cx.notify();

                return;
            }
            PaletteAction::RewindCheckpoint(checkpoint) => {
                let selected = self.session.borrow_mut().select_checkpoint(checkpoint);

                if selected {
                    self.palette.selected = 0;

                    cx.notify();
                }

                return;
            }
            PaletteAction::RewindAction(action) => {
                self.activate_rewind_action(action, cx);

                return;
            }
            PaletteAction::ForkCheckpoint(checkpoint) => {
                self.start_conversation_branch(checkpoint, cx);

                return;
            }
            PaletteAction::ForkCancel => {
                self.cancel_fork_picker(cx);

                return;
            }
        };

        self.input.update(cx, |input, cx| {
            input.set_value(text.clone(), window, cx);

            input.set_selected_range(text.len()..text.len(), cx);
        });

        self.palette.selected = 0;

        if execute && can_execute {
            self.submit_current_slash(window, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn add_response_annotation(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.attachments.add_annotation(text) {
            return;
        }

        TextSelection::clear(window, cx);

        self.focus(window, cx);

        cx.notify();
    }

    pub(crate) fn remove_response_annotation(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.attachments.remove_annotation(index) {
            cx.notify();
        }
    }

    pub(crate) fn submit_current_slash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.read(cx).text().to_string();

        if self.submit_slash_input(&input, cx) {
            self.input_history_navigation.record_input_history(
                &self.input_history_scope,
                &input,
                cx,
            );

            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));

            self.palette.dismissed = false;
            self.palette.selected = 0;
        }
    }

    /// Route a leading slash before ordinary message handling. Every failure
    /// returns false so the user's input stays available for correction.
    pub(super) fn submit_slash_input(&mut self, input: &str, cx: &mut Context<Self>) -> bool {
        let Some(session_host) = self.host.upgrade() else {
            return false;
        };

        let session_kind = session_host.read(cx).kind;

        if !self.binding.is_current() {
            return false;
        }

        let catalog = self.command_catalog(cx);
        let busy = self.is_command_busy();

        let Some(route) = route_slash(
            input,
            &catalog,
            session_kind.caps(),
            self.session.borrow().skill_catalog(),
            |name| self.command_choices(name, cx),
            busy,
        ) else {
            return false;
        };

        match route {
            SlashRoute::Refused(message) => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);

                false
            }
            SlashRoute::Prompt => self.send_text_inner(input.to_string(), None, None, cx),
            SlashRoute::Model(value) => {
                self.session.borrow_mut().controls.set_model(value.clone());

                remember_defaults(self, cx);

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    t!("agent-composer-model-set", value = &value).into_owned(),
                    cx,
                );

                // Where the harness adopts a model through its own request,
                // recording the pick is not applying it. This runs after the
                // notice so a refusal replaces it rather than hiding under
                // a confirmation of something that did not happen.
                self.apply_model_selection(cx);

                true
            }
            SlashRoute::Permissions(value) => {
                self.session.borrow_mut().controls.settings.approval = Some(value.clone());

                remember_defaults(self, cx);

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    t!(
                        "agent-composer-permissions-set",
                        value = &setting_value_label(&value)
                    )
                    .into_owned(),
                    cx,
                );

                true
            }
            SlashRoute::NewConversation => {
                self.reset_conversation(cx);

                true
            }
            SlashRoute::Resume => self.open_recent_sessions(cx),
            SlashRoute::Status => {
                let summary = {
                    let session = self.session.borrow();

                    status_summary(
                        session_kind,
                        session.runtime().status(),
                        &session.controls.settings,
                        session.commands().queue.len(),
                    )
                };

                // Answering /status is information the user asked for, so it
                // holds rather than fading out from under them.
                self.palette
                    .set_feedback(CommandFeedbackKind::Status, summary, cx);

                true
            }
            SlashRoute::Rewind => self.open_rewind(cx),
            SlashRoute::Rename(arguments) => self.rename_conversation(&arguments, cx),
            SlashRoute::Fork => self.open_fork(cx),
            SlashRoute::Find(arguments) => self.search_conversations(&arguments, cx),
            SlashRoute::Side(arguments) => self.ask_side_question(&arguments, cx),
            SlashRoute::Unapplied => false,
            SlashRoute::Backend { command, policy } => {
                self.route_backend_command(command, policy, cx)
            }
        }
    }

    pub(super) fn route_backend_command(
        &mut self,
        command: PendingSlashCommand,
        policy: SlashCommandRunPolicy,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.is_command_busy() {
            let admission = self
                .session
                .borrow_mut()
                .admit_command_while_busy(command, policy);

            return match admission {
                CommandAdmission::Queued { name, count } => {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Queued,
                        t!(
                            if count == 1 {
                                "agent-composer-command-queued-one"
                            } else {
                                "agent-composer-command-queued-many"
                            },
                            name = &name,
                            count = count
                        )
                        .into_owned(),
                        cx,
                    );

                    true
                }
                CommandAdmission::Busy { name } => {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        t!("agent-composer-command-idle-only", name = &name).into_owned(),
                        cx,
                    );

                    false
                }
                CommandAdmission::Execute(command) => self.execute_backend_command(command, cx),
            };
        }

        self.execute_backend_command(command, cx)
    }

    pub(crate) fn execute_backend_command(
        &mut self,
        command: PendingSlashCommand,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session_host) = self.host.upgrade() else {
            return false;
        };

        let session_kind = session_host.read(cx).kind;

        if !self.binding.is_current() {
            return false;
        }

        let outcome = self.session.borrow_mut().execute_command(&command);

        match outcome {
            SlashCommandOutcome::Accepted => {
                self.history_ui.mode = RecentSessionsMode::Hidden;

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    t!("agent-composer-command-starting", name = &command.name).into_owned(),
                    cx,
                );

                true
            }
            SlashCommandOutcome::Completed { message, approval } => {
                // The harness pins its own default preset into every
                // conversation it opens, so a switch it accepted is remembered
                // for the next one, whether it was picked or typed.
                if let Some(preset) = approval {
                    self.session.borrow_mut().controls.settings.approval = Some(preset);

                    remember_defaults(self, cx);
                }

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    message.unwrap_or_else(|| {
                        t!("agent-session-command-completed", name = &command.name).into_owned()
                    }),
                    cx,
                );

                true
            }
            SlashCommandOutcome::Rejected { message } => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);

                false
            }
            SlashCommandOutcome::NotReady => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    t!(
                        "agent-session-still-starting",
                        name = session_kind.display()
                    )
                    .into_owned(),
                    cx,
                );

                false
            }
        }
    }

    pub(super) fn skill_disabled_reason(&self, skill: &SkillInfo) -> Option<SharedString> {
        if !skill.enabled {
            Some(SharedString::from(t!("agent-composer-disabled-by-codex")))
        } else {
            // A skill is invoked through the harness, so it needs a session
            // that has finished starting and has not ended.
            match self.session.borrow().runtime().status() {
                Status::Starting => Some(SharedString::from(t!("agent-composer-agent-starting"))),
                Status::Exited => Some(SharedString::from(t!("agent-composer-agent-exited"))),
                _ => None,
            }
        }
    }

    pub(crate) fn command_catalog(&mut self, cx: &App) -> Rc<[SlashCommandInfo]> {
        let Some(session_host) = self.host.upgrade() else {
            return Rc::from([]);
        };

        let session_kind = session_host.read(cx).kind;

        let epoch = self.session.borrow().runtime().epoch();
        let language = rust_i18n::locale();

        if let Some(cached) = self
            .palette
            .catalog
            .as_ref()
            .filter(|cached| cached.language == *language && cached.epoch == epoch)
        {
            return cached.commands.clone();
        }

        let adapter = self
            .session
            .borrow()
            .runtime()
            .backend()
            .map(Backend::adapter_commands)
            .unwrap_or_else(|| adapter_commands(session_kind));

        let commands: Rc<[SlashCommandInfo]> = merge_catalog(
            local_commands(),
            adapter,
            self.session
                .borrow()
                .command_catalog()
                .unwrap_or_default()
                .to_vec(),
        )
        .into();

        self.palette.catalog = Some(CachedCatalog {
            language: language.to_string(),
            epoch,
            commands: commands.clone(),
        });

        commands
    }

    pub(super) fn command_choices(&self, command: &str, cx: &App) -> Vec<(String, String)> {
        let Some(session_host) = self.host.upgrade() else {
            return Vec::new();
        };

        let session_kind = session_host.read(cx).kind;

        match command {
            "model" => self
                .session
                .borrow()
                .controls
                .models
                .iter()
                .map(|model| (model.model.clone(), model.display.clone()))
                .collect(),
            "permissions" => match session_kind {
                AgentKind::Codex => app_server::APPROVAL_OPTIONS
                    .iter()
                    .map(|value| (value.to_string(), setting_value_label(value)))
                    .collect(),
                AgentKind::Claude => stream_json::PERMISSION_OPTIONS
                    .iter()
                    .map(|value| (value.to_string(), setting_value_label(value)))
                    .collect(),
                // Changing the DeepSeek sandbox preset mid-session is part of
                // the approval work, so the command offers no choices yet.
                AgentKind::DeepSeek => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn handle_input_history_control(
        &mut self,
        direction: InputHistoryDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let action = self.input_history_navigation.navigate_input(
            direction,
            &self.input,
            &self.input_history_scope,
            cx,
        );

        match action {
            InputHistoryAction::Declined => false,
            InputHistoryAction::Keep => {
                cx.stop_propagation();

                true
            }
            InputHistoryAction::Replace(text) => {
                self.palette.reset_for_recall();

                replace_input_with_history(&self.input, text, window, cx);

                cx.stop_propagation();

                cx.notify();

                true
            }
            InputHistoryAction::Clear => {
                self.input
                    .update(cx, |input, cx| input.set_value("", window, cx));

                cx.stop_propagation();

                cx.notify();

                true
            }
        }
    }

    pub(crate) fn present_questions(&mut self, index: usize, cx: &mut Context<Self>) {
        self.prompts.reveal(self.session.borrow().input(), index);

        cx.notify();
    }

    pub(crate) fn present_question_completion(
        &mut self,
        completion: QuestionCompletion,
        cx: &mut Context<Self>,
    ) {
        if completion.started_turn {
            self.start_working(cx);
        }

        self.prompts.hide_settled(self.session.borrow().input());

        self.transcript.update(cx, |_, cx| cx.notify());

        cx.notify();
    }

    pub(crate) fn open_message_questions(
        &mut self,
        item_id: &str,
        questions: Vec<Question>,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        self.prompts
            .open_history(self.session.borrow_mut().input_mut(), item_id, questions);

        cx.notify();
    }

    pub(crate) fn toggle_question_option(
        &mut self,
        question: usize,
        option: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        if self
            .prompts
            .toggle_option(self.session.borrow_mut().input_mut(), question, option)
        {
            cx.notify();
        }
    }

    pub(crate) fn submit_current_questions(&mut self, cx: &mut Context<Self>) {
        let key = self
            .prompts
            .questions(self.session.borrow().input())
            .map(|prompt| prompt.key());

        if let Some(key) = key {
            self.submit_question(key, QuestionAction::Answer, cx);
        }
    }

    pub(crate) fn skip_current_questions(&mut self, cx: &mut Context<Self>) {
        let key = self
            .prompts
            .questions(self.session.borrow().input())
            .map(|prompt| prompt.key());

        if let Some(key) = key {
            self.submit_question(key, QuestionAction::Skip, cx);
        }
    }

    fn submit_question(
        &mut self,
        key: QuestionKey,
        action: QuestionAction,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        let outcome = self
            .session
            .borrow_mut()
            .submit_question(key, action, Instant::now());

        match outcome {
            Submission::Ignored => return,
            Submission::Settled { waiting_finished } => {
                if waiting_finished {
                    self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                }
            }
            Submission::Waiting | Submission::Failed => {}
        }

        self.prompts.hide_settled(self.session.borrow().input());

        cx.notify();
    }

    pub fn refresh_background_tasks(&mut self) {
        self.session.borrow_mut().refresh_background_tasks();
    }

    /// Provider-qualified identity of the parent session child tasks belong to.
    /// `None` until the backend reports a thread or session id, which is what
    /// disables the title-bar `Background Tasks` button.
    pub fn background_task_parent(&self) -> Option<BackgroundTaskKey> {
        self.session.borrow().runtime().background_task_parent()
    }

    /// Ask the provider for one child's conversation. A provider that already
    /// has it, or that streams it live, does no work here.
    pub fn watch_background_task(
        &mut self,
        key: &BackgroundTaskKey,
        cx: &mut Context<Self>,
    ) -> Option<ChildReader> {
        self.host
            .upgrade()?
            .update(cx, |host, cx| host.watch_child(key, cx))
    }

    /// Stop one child agent, leaving this tab's own turn running. Reports
    /// whether the request was accepted, so the view can say so when a child
    /// turns out not to be stoppable after all — the snapshot a row was drawn
    /// from can be a moment behind the child finishing on its own.
    pub fn interrupt_background_task(&mut self, key: &BackgroundTaskKey) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        self.session.borrow_mut().interrupt_background_task(key)
    }

    /// One child's conversation, only while the pane still holds the session
    /// that child belongs to.
    pub fn background_task_transcript(
        &self,
        key: &BackgroundTaskKey,
    ) -> Option<Ref<'_, ChildTranscript>> {
        Ref::filter_map(self.session.borrow(), |session| {
            session.background_task_transcript(key)
        })
        .ok()
    }

    /// The latest snapshot, only while it still describes the session the pane
    /// currently holds. A snapshot left over from a replaced session is hidden
    /// rather than shown against the new parent.
    pub fn background_tasks(&self) -> Option<Ref<'_, BackgroundTaskSnapshot>> {
        Ref::filter_map(self.session.borrow(), |session| session.background_tasks()).ok()
    }

    /// Child agents of this tab the provider currently reports as active.
    pub fn running_background_tasks(&self) -> usize {
        self.background_tasks()
            .map(|snapshot| snapshot.active_count())
            .unwrap_or(0)
    }

    /// Child agents this tab has, running and finished alike. A finished child
    /// is still something to open the view for, so the chrome asks for this
    /// rather than the running count when deciding to offer the control.
    pub fn background_task_count(&self) -> usize {
        self.background_tasks()
            .map(|tasks| tasks.tasks.len())
            .unwrap_or(0)
    }

    /// Pin a title on this conversation.
    ///
    /// An empty title is refused here rather than sent, because a backend that
    /// normalizes it away answers the same refusal after a round trip and the
    /// composer would have discarded the line in the meantime.
    pub(crate) fn rename_conversation(&mut self, title: &str, cx: &mut Context<Self>) -> bool {
        let Some(session_host) = self.host.upgrade() else {
            return false;
        };

        let session_kind = session_host.read(cx).kind;

        if !self.binding.is_current() {
            return false;
        }

        let title = title.trim();

        if title.is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-rename-needs-title").to_string(),
                cx,
            );

            return false;
        }

        let outcome = match self.session.borrow_mut().rename_conversation(title) {
            Some(outcome) => outcome.map_err(operation_error),
            None => Err(t!(
                "agent-session-still-starting",
                name = session_kind.display()
            )
            .into_owned()),
        };

        // The backend answers with the title it was asked for. What it keeps
        // after its own normalization reaches the tab as a title update, and
        // a refusal is reported in the transcript.
        match outcome {
            Ok(accepted) => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    t!("agent-session-renamed", title = &accepted).into_owned(),
                    cx,
                );

                true
            }
            Err(error) => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, error, cx);

                false
            }
        }
    }

    /// Ask the backend which earlier conversations mention a phrase.
    ///
    /// The answer replaces the recent list, so the list is opened here and the
    /// arriving results land in a surface the user is already looking at
    /// rather than one they would have to go and find.
    pub(crate) fn search_conversations(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        let Some(session_host) = self.host.upgrade() else {
            return false;
        };

        let session_kind = session_host.read(cx).kind;

        if !self.binding.is_current() {
            return false;
        }

        let query = query.trim();

        if query.is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-search-needs-query").to_string(),
                cx,
            );

            return false;
        }

        if !self.session.borrow_mut().search_sessions(query) {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!(
                    "agent-session-still-starting",
                    name = session_kind.display()
                )
                .into_owned(),
                cx,
            );

            return false;
        }

        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            t!("agent-session-searching", query = query).into_owned(),
            cx,
        );

        true
    }

    /// Ask a question beside the conversation. The answer lands in the Side
    /// Chat window, which opens, or comes back from being minimized, with the
    /// question itself.
    pub(crate) fn ask_side_question(&mut self, question: &str, cx: &mut Context<Self>) -> bool {
        let Some(session_host) = self.host.upgrade() else {
            return false;
        };

        let session_kind = session_host.read(cx).kind;

        if !self.binding.is_current() {
            return false;
        }

        let question = question.trim();

        let refusal = if question.is_empty() {
            t!("agent-side-needs-question").into_owned()
        } else {
            let outcome = self
                .session
                .borrow_mut()
                .ask_side_question(question.to_owned());

            match outcome {
                SideQuestionOutcome::Asked => {
                    self.side_chat.minimized = false;

                    self.sync_side_chat(cx);

                    return true;
                }
                SideQuestionOutcome::Busy => t!("agent-side-busy").into_owned(),
                SideQuestionOutcome::Unsupported => {
                    t!("agent-side-unsupported", name = session_kind.display()).into_owned()
                }
                SideQuestionOutcome::Failed(error) => {
                    t!("agent-side-failed", error = &error).into_owned()
                }
            }
        };

        self.palette
            .set_feedback(CommandFeedbackKind::Error, refusal, cx);

        false
    }

    /// Ask before discarding the side chat: closing drops every exchange, and
    /// nothing of it was ever part of the conversation to find again.
    pub(crate) fn confirm_close_side_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = cx.entity();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let pane = pane.clone();

            alert
                .confirm()
                .title(t!("agent-side-close-title"))
                .description(t!("agent-side-close-description").into_owned())
                .on_ok(move |_, _, cx| {
                    pane.update(cx, |pane, cx| pane.close_side_chat(cx));

                    true
                })
        });
    }

    fn close_side_chat(&mut self, cx: &mut Context<Self>) {
        self.session.borrow_mut().close_side_questions();

        self.side_chat.minimized = false;

        self.sync_side_chat(cx);
    }

    /// Minimize the Side Chat window, or bring it back. Answers keep arriving
    /// while it is minimized.
    pub fn toggle_side_chat(&mut self, cx: &mut Context<Self>) {
        if !self.session.borrow().side_questions().is_open() {
            return;
        }

        self.side_chat.minimized = !self.side_chat.minimized;

        self.sync_side_chat(cx);
    }

    /// Whether the Side Chat window is showing, or `None` while there is no
    /// side chat to show.
    pub fn side_chat_shown(&self) -> Option<bool> {
        self.session
            .borrow()
            .side_questions()
            .is_open()
            .then_some(!self.side_chat.minimized)
    }

    /// Bring the side transcript up to date and tell the chrome, whose Side
    /// Chat control follows whether the window exists and is showing.
    fn sync_side_chat(&mut self, cx: &mut Context<Self>) {
        self.side_chat.transcript.update(cx, |view, cx| {
            view.sync_content();

            cx.notify();
        });

        self.emit_event(AgentPaneEvent::SideChatActivity, cx);

        cx.notify();
    }

    /// Show what one search matched, in place of whatever the list held.
    ///
    /// An empty result set keeps the list closed and says so, because opening
    /// an empty strip would read as a list that failed to load.
    pub(crate) fn show_search_results(
        &mut self,
        results: Vec<SessionSummary>,
        cx: &mut Context<Self>,
    ) {
        let count = results.len();

        if !self.history_ui.data.search_results(results) {
            self.palette.set_feedback(
                CommandFeedbackKind::Notice,
                t!("agent-session-search-no-matches").to_string(),
                cx,
            );

            return;
        }

        self.history_ui.selected = 0;
        self.history_ui.mode = RecentSessionsMode::Open;

        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            t!("agent-session-search-matches", count = count).into_owned(),
            cx,
        );
    }

    /// Drop one prompt waiting behind the running turn.
    ///
    /// The row stays until the backend confirms the removal: a message it has
    /// already claimed is one the transcript is about to show as sent, and
    /// removing the row first would make it look like it never went.
    pub(crate) fn remove_queued_prompt(&mut self, item_id: &str, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if !self.session.borrow_mut().withdraw_queued_prompt(item_id) {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-queued-remove-failed").to_string(),
                cx,
            );

            return;
        }

        cx.notify();
    }

    pub(super) fn present_session_effect(&mut self, effect: SessionEffect, cx: &mut Context<Self>) {
        // A pane outliving its session has no effect left to present.
        if self.host.upgrade().is_none() {
            return;
        }

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

        self.side_chat.transcript.update(cx, |transcript, cx| {
            transcript.sync_content();

            cx.notify();
        });

        match effect {
            SessionEffect::Unchanged => {}
            SessionEffect::ProviderTurnAccepted { .. } => {}
            SessionEffect::TeamDecision(_) => {}
            SessionEffect::ProviderTurnFinished { .. } => {}
            SessionEffect::Changed => cx.notify(),
            SessionEffect::Title(_) => {}
            SessionEffect::Ready(settings) => self.on_ready(settings, cx),
            SessionEffect::Commands => {
                self.palette.catalog = None;
                self.palette.selected = 0;

                cx.notify();
            }
            SessionEffect::Skills => {
                self.palette.selected = 0;

                cx.notify();
            }
            SessionEffect::CommandResult { name, outcome } => {
                self.on_slash_command_result(&name, outcome, cx);
            }
            SessionEffect::TurnStarted { opened } => self.on_turn_started(opened, cx),
            SessionEffect::TurnCompleted { .. } => self.on_turn_completed(cx),
            SessionEffect::StatusDetail(_) => cx.notify(),
            SessionEffect::ApprovalRequested | SessionEffect::ApprovalResolved => cx.notify(),
            SessionEffect::InputRequested { index } => self.present_questions(index, cx),
            SessionEffect::InputResolved(completion) => {
                self.present_question_completion(completion, cx)
            }
            SessionEffect::Workflows { .. } | SessionEffect::BackgroundActivity => cx.notify(),
            SessionEffect::Branch(update @ BranchUpdate::Branching) => {
                self.on_fork_update(update, cx)
            }
            SessionEffect::Branch(update) => self.on_rewind_update(update, cx),
            SessionEffect::Error {
                message,
                fatal,
                failure,
            } => self.on_error(message, fatal, failure, cx),
            SessionEffect::EffortRejected { message } => {
                remember_defaults(self, cx);

                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
            }
            SessionEffect::History(sessions) => self.on_history(sessions, cx),
            SessionEffect::SearchResults(results) => self.show_search_results(results, cx),
            SessionEffect::Replay(replay) => {
                if let Some(completion) = replay.branch {
                    self.complete_branch(completion, cx);
                }

                if replay.replace || self.history_ui.mode == RecentSessionsMode::Loading {
                    self.clear_conversation_presentation(cx);

                    self.history_ui.mode = RecentSessionsMode::Hidden;
                    self.palette.feedback = None;
                }

                self.transcript
                    .update(cx, |transcript, _| transcript.sync_content());

                if let Some(title) = replay.title {
                    self.emit_event(AgentPaneEvent::TitleSuggested(title), cx);
                }
            }
        }
    }

    /// Handshake finished. Fold the reported thread settings together with
    /// remembered picks, settle status, and rebuild child state from history.
    fn on_ready(&mut self, ready: SessionReady, cx: &mut Context<Self>) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;
        let session_profile = session_host.read(cx).profile.clone();

        if let Some(completion) = ready.branch {
            self.complete_branch(completion, cx);
        }

        if ready.replaced {
            self.clear_conversation_presentation(cx);

            self.history_ui.mode = RecentSessionsMode::Hidden;
            self.palette.feedback = None;
        }

        if let Some(title) = ready.title {
            self.emit_event(AgentPaneEvent::TitleSuggested(title), cx);
        }

        let selection = ready.selection;

        self.prompts.reset_editors();

        if let Some(SettingsOutcome::Refused { message }) = selection {
            self.palette
                .set_feedback(CommandFeedbackKind::Error, message, cx);
        }

        if let Some(SettingsOutcome::Refused { message }) = ready.approval {
            self.palette
                .set_feedback(CommandFeedbackKind::Error, message, cx);
        }

        info!(
            "agent thread ready: profile=\"{}\", model={:?}, profile_model={:?}",
            session_profile.name,
            self.session.borrow().controls.settings.model,
            launch_model(session_kind, &session_profile)
        );

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
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

        match outcome {
            SlashCommandOutcome::Accepted => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    t!("agent-session-command-accepted", name = name).into_owned(),
                    cx,
                );
            }
            SlashCommandOutcome::Completed { message, approval } => {
                // A backend that answers its commands later reports an
                // accepted permission switch here, and it is remembered for
                // the next conversation the same way an immediate answer is.
                if let Some(preset) = approval {
                    self.session.borrow_mut().controls.settings.approval = Some(preset);

                    remember_defaults(self, cx);
                }

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    message.unwrap_or_else(|| {
                        t!("agent-session-command-completed", name = name).into_owned()
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
                    t!(
                        "agent-session-provider-not-ready",
                        name = session_kind.display()
                    )
                    .into_owned(),
                    cx,
                );
            }
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

        cx.notify();
    }

    fn on_turn_completed(&mut self, cx: &mut Context<Self>) {
        self.turn.refresh_timer(cx);

        self.refresh_git_branch(cx);

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
        let resume_failed = failure.resume_failed;

        if resume_failed || self.history_ui.mode == RecentSessionsMode::Loading {
            self.history_ui.mode = RecentSessionsMode::Open;

            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-open-failed", error = &message).into_owned(),
                cx,
            );
        }

        if let Some(failure) = failure.branch {
            self.report_branch_failure(failure, cx);
        }

        if fatal {
            self.prompts
                .release_secret_editors(self.session.borrow().input());

            self.publish_queued_user_messages(cx);
        }

        if failure.cancelled_commands {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-queued-cancelled-failed").to_string(),
                cx,
            );
        }
    }

    /// A list of what is recent answers a different question than the search
    /// currently on screen, so it replaces those rows rather than being
    /// appended to them.
    fn on_history(&mut self, sessions: Vec<SessionSummary>, cx: &mut Context<Self>) {
        self.history_ui.data.append_page(sessions);

        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn start_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        self.session.borrow_mut().start_item(item);

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

        cx.notify();
    }

    pub(super) fn publish_queued_user_messages(&mut self, cx: &mut Context<Self>) {
        self.session.borrow_mut().publish_confirmed();

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());
    }

    #[cfg(test)]
    pub(crate) fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        field: TextField,
        cx: &mut Context<Self>,
    ) {
        self.session
            .borrow_mut()
            .append_delta(item_id, delta, field);

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

        cx.notify();
    }

    /// Widen the session list to every directory, or narrow it back to this
    /// tab's. The rows on screen answered the previous scope, so they go; the
    /// reload republishes what the new one covers. A backend that lists over
    /// the protocol is asked again, one that reads its own transcripts is
    /// rescanned.
    pub(crate) fn toggle_history_scope(&mut self, cx: &mut Context<Self>) {
        let scope = self.history_ui.toggle_scope();

        self.session.borrow_mut().request_history(scope);

        self.load_filesystem_history(cx);

        cx.notify();
    }

    /// History read from the CLI's transcript directory, for a harness that
    /// does not deliver it over the protocol as `Event::History`. Two passes,
    /// both off-thread: a cheap count first, so the list can reserve its final
    /// height with placeholder rows, then title parsing, which swaps in the
    /// real rows.
    fn load_filesystem_history(&mut self, cx: &mut Context<Self>) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

        if !session_kind.caps().filesystem_session_history {
            return;
        }

        let cwd = self.cwd(cx);

        self.history_ui.load_filesystem_history(cwd, cx);
    }

    fn seed_restored_settings(&mut self, seed: SettingsSeed) {
        if !self.binding.is_current() {
            return;
        }

        self.session.borrow_mut().controls.seed_settings(seed);
    }

    /// Keep the displayed conversation until the replacement supplies its replay.
    pub(crate) fn resume_session(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

        if !self.binding.is_current() {
            return;
        }

        let Some(summary) = self.history_ui.data.sessions.get(index) else {
            return;
        };

        // Both operations replace the conversation; a visible history list
        // must not start a resume while a branch picker or file step owns it.
        if self.history_ui.mode == RecentSessionsMode::Loading
            || self.session.borrow().branch().holds_composer()
        {
            return;
        }

        let cwd = self.cwd(cx);

        let outcome = self
            .session
            .borrow_mut()
            .begin_resume(summary, cwd.as_deref());

        let request = match outcome {
            ResumeStart::Busy => return,
            ResumeStart::Elsewhere { cwd, .. } => {
                // The whole row goes along, so the tab that opens the
                // conversation is named after it like one resumed here.
                let summary = summary.clone();

                self.history_ui.selected = index;

                self.emit_event(AgentPaneEvent::ResumeElsewhere { cwd, summary }, cx);

                cx.notify();

                return;
            }
            ResumeStart::Rejected => {
                self.history_ui.mode = RecentSessionsMode::Open;
                self.history_ui.selected = index;

                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    t!("agent-session-codex-recent-not-ready").to_string(),
                    cx,
                );

                return;
            }
            ResumeStart::Requested => {
                self.seed_restored_settings(SettingsSeed::resumed(session_kind));

                None
            }
            ResumeStart::ReadReplay(request) => Some(request),
        };

        self.history_ui.mode = RecentSessionsMode::Loading;
        self.history_ui.selected = index;

        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            t!("agent-session-opening-recent").to_string(),
            cx,
        );

        let Some(request) = request else {
            return;
        };

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.read_resume(request, cx));
        }
    }

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
        let owner = AgentSession::create(profile, workspace, None, cx);

        let mut pane = Self::attach(&owner, window, cx);

        pane.owned_session = Some(owner);

        pane.start_session_with_options(resume, false, |_, _, _| {}, cx);

        pane
    }

    pub(super) fn attach_team_member(
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
        let binding = owner.bind();
        let kind = profile.kind;
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

            transcript.attach_content(session.borrow().conversation().clone(), cx);

            transcript.set_owner(owner);

            transcript
        });

        // The side chat renders through its own view: a conversation's
        // measured rows and scroll position belong to one list each. Like a
        // child agent's view it has no owner, because branching and rewinding
        // address the main conversation, which none of its prompts opened.
        let side_transcript = cx.new(|cx| {
            let mut transcript = TranscriptView::new(kind, cwd.clone());

            transcript.attach_content(session.borrow().side_questions().conversation().clone(), cx);

            transcript
        });

        let mut this = Self {
            focus: cx.focus_handle(),
            input_history_scope,
            input_history_navigation: InputHistoryNavigation::default(),
            attachments: ComposerAttachments::default(),
            transcript,
            input,
            session,
            host: host.downgrade(),
            binding,
            team_member: false,
            #[cfg(test)]
            owned_session: None,
            history_ui: SessionHistoryUi::default(),
            progress_panel: ProgressPanel::default(),
            side_chat: SideChatWindow::new(side_transcript),
            prompts: QuestionPanel::default(),
            effort_drag: None,
            turn: TurnPresentation::default(),
            palette: SlashPalette::default(),
            branch: BranchFlow::default(),
            composer_status: ComposerStatusBar::default(),
            workflows: WorkflowUi::default(),
            blocking_overlay: BlockingOverlay::default(),
        };

        {
            let state = this.session.borrow();

            for index in 0..state.input().batches().len() {
                this.prompts.reveal(state.input(), index);
            }

            this.prompts.hide_settled(state.input());
        }

        this.turn.refresh_timer(cx);

        if this.transcript.read(cx).is_working() {
            this.start_working(cx);
        }

        cx.observe(host, |this, _, cx| {
            this.transcript.update(cx, |view, cx| {
                view.sync_content();

                cx.notify();
            });

            this.side_chat.transcript.update(cx, |view, cx| {
                view.sync_content();

                cx.notify();
            });

            cx.notify();
        })
        .detach();

        cx.subscribe(host, |this, _, event: &PresentationEffect, cx| {
            if this.binding.is_current()
                && this.binding.generation == event.generation
                && this.session.borrow().runtime().is_current(event.epoch)
                && let Some(effect) = event.effect.borrow_mut().take()
            {
                this.present_session_effect(effect, cx);
            }
        })
        .detach();

        this.refresh_git_branch(cx);

        cx.spawn(poll_git_branch).detach();

        this.load_filesystem_history(cx);

        this
    }

    pub fn agent_route<'a>(&self, cx: &'a App) -> Option<&'a AgentRoute> {
        Some(&self.host.upgrade()?.read(cx).route)
    }

    pub fn agent_kind(&self, cx: &App) -> Option<AgentKind> {
        Some(self.host.upgrade()?.read(cx).kind)
    }

    /// The tab's primary directory, which transcript links resolve against.
    pub fn working_directory(&self, cx: &App) -> Option<String> {
        self.cwd(cx)
    }

    /// The tab's primary directory: where its process runs, what its provider
    /// session history is scoped to, and what a relative path resolves against.
    pub(super) fn cwd(&self, cx: &App) -> Option<String> {
        self.host
            .upgrade()?
            .read(cx)
            .workspace
            .primary()
            .map(str::to_string)
    }

    /// The directories this tab is currently configured with. A conversation
    /// started from now on receives these.
    pub(super) fn configured_workspace<'a>(&self, cx: &'a App) -> Option<&'a AgentWorkspace> {
        Some(&self.host.upgrade()?.read(cx).workspace)
    }

    /// Replace the configured directory list after the parent workspace was
    /// edited. The running conversation keeps the snapshot it started with;
    /// the next one clones this.
    pub fn set_workspace(&mut self, workspace: AgentWorkspace, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        let Some(host) = self.host.upgrade() else {
            return;
        };

        if host.read(cx).workspace == workspace {
            return;
        }

        let primary_changed = host.read(cx).workspace.primary() != workspace.primary();

        self.input_history_scope = InputHistoryScope::local(host.read(cx).kind, &workspace);

        host.update(cx, |host, _| host.workspace = workspace);

        if primary_changed {
            self.composer_status.invalidate_branch();

            self.refresh_git_branch(cx);
        }

        cx.notify();
    }

    /// Append one item to the conversation, tagged with the current turn so
    /// settled turns fold as one unit. The controller owns the conversation,
    /// so the item goes through it and the view picks the change up the same
    /// way it picks up every other one.
    pub(super) fn push_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        self.session.borrow_mut().push_item(item);

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

        cx.notify();
    }

    pub(super) fn refresh_git_branch(&mut self, cx: &mut Context<Self>) {
        let cwd = self.cwd(cx).or_else(|| {
            env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        });

        self.composer_status.refresh_branch(cwd, cx);
    }

    pub fn agent_session(&self) -> Option<Entity<AgentSession>> {
        self.host.upgrade()
    }

    pub(super) fn emit_event(&self, event: AgentPaneEvent, cx: &mut Context<Self>) {
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
        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.emit_lifecycle(kind, title, body, cx));
        }
    }

    #[cfg(test)]
    pub(super) fn latest_agent_message(&self, cx: &App) -> Option<String> {
        self.transcript
            .read(cx)
            .latest_agent_message(self.session.borrow().turn())
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn kind(&self, cx: &App) -> Option<AgentKind> {
        Some(self.host.upgrade()?.read(cx).kind)
    }

    /// Progress through the task list this conversation is working from, as
    /// completed items out of the total, for the workspace entry's bar.
    pub fn task_tally(&self, cx: &App) -> Option<(u32, u32)> {
        match self.session.borrow().task_list() {
            Some(tasks) => tasks.tally(),
            None => self.transcript.read(cx).task_tally(),
        }
    }

    /// The launch profile this pane runs, so a tab opened from one of its
    /// rows launches the same agent with the same configuration.
    pub fn profile<'a>(&self, cx: &'a App) -> Option<&'a AgentProfile> {
        Some(&self.host.upgrade()?.read(cx).profile)
    }

    /// Name of the launch profile, persisted with the tab snapshot so
    /// restore reopens the same profile.
    pub fn profile_name<'a>(&self, cx: &'a App) -> Option<&'a str> {
        Some(&self.host.upgrade()?.read(cx).profile.name)
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

    pub(super) fn send_text_inner(
        &mut self,
        text: String,
        skill: Option<&SkillReference>,
        restore_on_interrupt: Option<(String, Vec<String>)>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.attachments.preparing > 0 {
            return false;
        }

        let Some(session_host) = self.host.upgrade() else {
            return false;
        };

        let session_kind = session_host.read(cx).kind;

        if !self.binding.is_current() {
            return false;
        }

        let title_text = restore_on_interrupt
            .as_ref()
            .map_or(text.as_str(), |(prompt, _)| prompt.as_str());

        let title_request = self
            .session
            .borrow()
            .title_request(title_text, tab_title_from_prompt);

        let settings = self.session.borrow().controls.settings.clone();
        let image_paths = self.attachments.paths();

        let restores_annotations = restore_on_interrupt.is_some();

        let image_prompt = (!self.attachments.images().is_empty()).then(|| text.clone());

        let outcome = self.session.borrow_mut().submit(
            text.clone(),
            |session, text| {
                let images: Vec<_> = self
                    .attachments
                    .images()
                    .iter()
                    .map(|image| ImageAttachment {
                        bytes: image.image.bytes(),
                        media_type: image.image.format().mime_type(),
                    })
                    .collect();

                session.submit(&PromptRequest {
                    text,
                    settings: &settings,
                    skill,
                    images: &images,
                    image_paths: &image_paths,
                    title: title_request.as_ref(),
                })
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
            Ok(SendOutcome::StartedTurn) => Some(text),
            Ok(SendOutcome::Steered) => None,
            Ok(SendOutcome::NotReady) => {
                self.push_item(
                    SessionItem::Error {
                        text: t!(
                            "agent-session-still-starting",
                            name = session_kind.display()
                        )
                        .into_owned(),
                    },
                    cx,
                );

                return false;
            }
            Ok(SendOutcome::Rejected { message }) => {
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
        if matches!(session_kind, AgentKind::Codex | AgentKind::Claude)
            && let Some(title) = title_request
        {
            self.session.borrow_mut().claim_title();

            self.emit_event(AgentPaneEvent::TitleSuggested(title.provisional_title), cx);
        }

        // Accepted: the images went with it, so the transcript keeps them and
        // the composer lets them go. A refusal above keeps them pending, so
        // the message stays as recoverable as its text.
        let sent_images: Vec<Arc<Image>> = self
            .attachments
            .images()
            .iter()
            .map(|attachment| attachment.image.clone())
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
                let shared = self.session.borrow().conversation().clone();

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

                    self.session.borrow_mut().hold_sent_images(text, images);
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

        self.history_ui.data.invalidate_filesystem_history();

        // Workflow runs are scoped the same way, and their refresh must not
        // keep polling a directory that belongs to the replaced conversation.
        self.workflows.clear_workflows();

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

        self.session.borrow_mut().rename(title);
    }

    pub(super) fn reset_conversation(&mut self, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.reset(cx));
        }

        self.clear_conversation_presentation(cx);

        self.palette.skill_binding = None;

        self.palette.reset_discovery();

        self.session.borrow_mut().clear_commands();

        self.palette.feedback = None;
        self.history_ui.mode = RecentSessionsMode::Hidden;

        cx.notify();
    }

    pub(crate) fn shows_start_overlay(&self) -> bool {
        self.session.borrow().runtime().status() == Status::Starting
    }

    pub(crate) fn start_session(&mut self, resume: Option<String>, cx: &mut Context<Self>) {
        self.start_session_with_options(
            resume.map(|id| RecoveryIdentity::new(AgentKind::Claude, id)),
            false,
            |_, _, _| {},
            cx,
        );
    }

    pub(crate) fn start_session_with_options(
        &mut self,
        recovery: Option<RecoveryIdentity>,
        preserve_settings: bool,
        on_result: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        let Some(host) = self.host.upgrade() else {
            return;
        };

        self.history_ui.data.invalidate_filesystem_history();

        self.palette.skill_binding = None;

        let pane = cx.entity().downgrade();

        host.update(cx, |host, cx| {
            host.start(
                recovery,
                preserve_settings,
                move |started, cx| {
                    let _ = pane.update(cx, |pane, cx| {
                        if pane.binding.is_current() {
                            on_result(pane, started, cx);

                            cx.notify();
                        }
                    });
                },
                cx,
            );
        });

        self.prompts
            .release_secret_editors(self.session.borrow().input());

        if !self.session.borrow().branch().holds_composer() {
            self.branch.clear();
        }

        cx.notify();
    }

    /// Show the live progress row the controller just opened and drive its
    /// once-a-second repaint; the ticker stops itself once the turn settles.
    pub(crate) fn start_working(&mut self, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

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

    pub(crate) fn interrupt_from_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        let interrupted = self.session.borrow_mut().interrupt_from_user();

        if let Some((_, prompt)) = interrupted.prompt {
            // The controller already dropped the turn that produced nothing.
            self.transcript
                .update(cx, |transcript, _| transcript.sync_content());

            let current = self.input.read(cx).text().to_string();
            let restored = restored_input_after_interruption(&prompt.text, &current);
            let cursor = restored.len();

            self.input.update(cx, |input, cx| {
                input.set_value(restored, window, cx);

                input.set_selected_range(cursor..cursor, cx);
            });

            self.palette.skill_binding = prompt.skill;

            self.attachments
                .restore_annotations(prompt.response_annotations);

            cx.notify();
        }

        self.present_interrupt_result(interrupted.outcome, cx);
    }

    fn present_interrupt_result(&mut self, outcome: InterruptOutcome, cx: &mut Context<Self>) {
        match outcome {
            InterruptOutcome::Unavailable => {}
            InterruptOutcome::Rejected => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    "The interrupt request could not be queued.",
                    cx,
                );
            }
            InterruptOutcome::Accepted => {
                self.emit_event(AgentPaneEvent::Interrupted, cx);

                cx.notify();
            }
        }
    }

    pub(crate) fn respond_approval(&mut self, decision: &str, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        let outcome = self.session.borrow_mut().respond_approval(decision);

        match outcome {
            ApprovalOutcome::Ignored => return,
            ApprovalOutcome::Settled => {
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
            }
            ApprovalOutcome::Waiting => {}
            ApprovalOutcome::Rejected => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    "The approval response could not be queued.",
                    cx,
                );
            }
        }

        cx.notify();
    }

    pub fn recovery_readiness(&self, cx: &App) -> RecoveryReadiness {
        self.host.upgrade().map_or_else(
            || RecoveryReadiness::Busy("session closed".into()),
            |host| host.read(cx).recovery_readiness(),
        )
    }

    pub(crate) fn retry_update_recovery(&mut self, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.retry_update_recovery(cx));
        }
    }

    pub(crate) fn start_new_after_update_failure(&mut self, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.start_new_after_update_failure(cx));
        }
    }

    /// Push the current model and effort picks to a harness that applies them
    /// as their own request.
    ///
    /// A refusal restores both pickers from what the session is actually set
    /// to, because a picker left showing a value the harness never adopted
    /// would misreport which model the next turn runs on.
    pub(crate) fn apply_model_selection(&mut self, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        let Some(outcome) = self.session.borrow_mut().apply_model_selection() else {
            return;
        };

        match outcome {
            SettingsOutcome::Effective
            | SettingsOutcome::Requested
            | SettingsOutcome::RidesNextSubmission => cx.notify(),
            SettingsOutcome::Refused { message } => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx)
            }
        }
    }

    /// Rebuild this conversation's agent from another composition.
    ///
    /// The harness allows this only before the conversation has run anything,
    /// because the logged history was produced under the previous composition's
    /// tools. That rule is not repeated here: the picker reports whatever the
    /// harness answers, and the row stays on the preset still in force.
    pub(crate) fn apply_agent_preset(&mut self, preset: String, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        let Some(outcome) = self.session.borrow_mut().select_agent_preset(preset) else {
            return;
        };

        match outcome {
            SettingsOutcome::Effective
            | SettingsOutcome::Requested
            | SettingsOutcome::RidesNextSubmission => {
                // The harness composes an agent only when a conversation is
                // created, so the pick is remembered for the next creation.
                remember_defaults(self, cx);

                cx.notify()
            }
            SettingsOutcome::Refused { message } => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx)
            }
        }
    }

    pub(super) fn render_approval_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session_kind = self.host.upgrade()?.read(cx).kind;

        let description = self
            .session
            .borrow()
            .input()
            .approval()
            .map(str::to_owned)?;

        Some(approval_card(
            description,
            session_kind.caps().session_scoped_approval,
            cx,
        ))
    }

    /// Empty the composer once the Team has taken what it held.
    pub(crate) fn clear_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    /// What this Team member is waiting on the user for: an approval, then
    /// the questions it asked. Empty while it asks nothing. The Team view
    /// draws these in its own composer as well as in the member's view, so
    /// an answer never depends on which of the two is open.
    pub(crate) fn render_team_interactions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let composer_free = !self.branch_flow_holds_composer();

        self.render_approval_panel(cx)
            .into_iter()
            .chain(
                self.prompts
                    .render(&self.session, composer_free, window, cx),
            )
            .collect()
    }

    /// A strip naming the workspace directories the installed harness cannot
    /// use, when there are any.
    pub(super) fn render_multi_root_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session_kind = self.host.upgrade()?.read(cx).kind;

        let notice = multi_root_notice(session_kind, self.configured_workspace(cx)?)?;

        Some(multi_root_strip(notice, cx))
    }

    pub(super) fn render_composer_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let session = self.session.borrow();
        let steps = self.transcript.read(cx).turn_steps(session.turn());

        self.composer_status.render(&session, steps, cx)
    }

    fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        // A branch or rewind picker owns Escape ahead of anything
        // under it, and closing one changes nothing else.
        if self.cancel_branch_picker(cx) {
        } else if self.session.borrow().input().approval().is_some() {
            self.respond_approval("cancel", cx);
        } else if self.prompts.questions_open(self.session.borrow().input()) {
            self.prompts.collapsed = true;

            cx.notify();
        } else if self.session.borrow().runtime().status() == Status::Running {
            self.interrupt_from_ui(window, cx);
        }
    }

    fn on_transcript_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus(window, cx);

        let pane = cx.entity().downgrade();

        // Kept as the fallback anchor for a selection whose
        // rect cannot be resolved, so the menu still opens
        // somewhere the pointer just was.
        let released_at = event.position;

        window.on_next_frame(move |window, cx| {
            show_selected_text_menu(pane, released_at, window, cx);
        });

        cx.notify();
    }

    /// How long ago the agent last answered, as a mark beside the composer's
    /// controls. Absent until a turn has settled, and while one is running:
    /// the transcript's own live "Working for" reading is the answer then, and
    /// two clocks a few pixels apart would be read as disagreeing.
    fn render_last_response(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at = self
            .session
            .borrow()
            .conversation()
            .borrow()
            .last_response_at?;

        if self.transcript.read(cx).is_working() {
            return None;
        }

        last_response_mark(at.elapsed().as_secs(), cx)
    }

    /// Session id when this pane runs a harness that reports workflows, which
    /// is what scopes runs to the conversation they belong to.
    pub fn workflow_session_id(&self, cx: &App) -> Option<String> {
        let session_host = self.host.upgrade()?;

        let session_kind = session_host.read(cx).kind;

        if !session_kind.caps().workflows {
            return None;
        }

        self.session
            .borrow()
            .runtime()
            .backend()
            .and_then(Backend::session_id)
            .map(str::to_owned)
    }

    /// Runs of the scoped session, in provider order.
    pub fn workflow_runs(&self) -> Ref<'_, [WorkflowRun]> {
        Ref::map(self.session.borrow(), |session| session.workflows().runs())
    }

    /// Agents of this tab the provider currently reports as running.
    pub fn running_workflow_agents(&self) -> usize {
        self.session.borrow().workflows().running_agents()
    }

    /// Rows for a skill query, shared by the `/` picker stage and the `$`
    /// prefix. Discovery runs in the background, so a missing catalog is a
    /// loading state rather than an empty result.
    fn skill_palette_model(&self, query: &str) -> PaletteModel {
        let session = self.session.borrow();

        let Some(skill_catalog) = session.skill_catalog() else {
            return PaletteModel {
                rows: Vec::new(),
                note: Some(SharedString::from(t!(
                    "agent-composer-skill-discovery-loading"
                ))),
            };
        };

        let rows = filter_skill_catalog(&skill_catalog.skills, query)
            .into_iter()
            .map(|skill| PaletteRow {
                label: format!("${}", skill.name).into(),
                description: SharedString::new(&skill.description),
                hint: Some(SharedString::new(&skill.scope)),
                disabled_reason: self.skill_disabled_reason(&skill),
                action: PaletteAction::Skill(skill),
            })
            .collect::<Vec<_>>();

        let note = if rows.is_empty() && !skill_catalog.errors.is_empty() {
            Some(SharedString::new(&skill_catalog.errors[0]))
        } else if rows.is_empty() && query.is_empty() {
            Some(SharedString::from(t!("agent-composer-no-skills")))
        } else if rows.is_empty() {
            Some(SharedString::from(t!("agent-composer-no-matching-skills")))
        } else {
            skill_catalog.errors.first().map(|error| {
                t!("agent-composer-skill-load-partial", error = error)
                    .into_owned()
                    .into()
            })
        };

        PaletteModel { rows, note }
    }

    pub fn set_workflows_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.workflows
            .set_workflows_visible(&self.host, visible, cx);
    }

    pub fn open_workflow_conversation(&self) -> Option<Ref<'_, OpenWorkflowAgent>> {
        self.workflows.open_workflow_conversation(&self.session)
    }

    pub fn open_workflow_agent(&mut self, task_id: &str, agent_id: &str, cx: &mut Context<Self>) {
        self.workflows
            .open_workflow_agent(&self.session, &self.host, task_id, agent_id, cx);
    }

    pub fn close_workflow_agent(&mut self, cx: &mut Context<Self>) {
        self.workflows.close_workflow_agent(cx);
    }
}

impl gpui::EventEmitter<AgentPaneEvent> for AgentPane {}

impl gpui::Focusable for AgentPane {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(session_host) = self.host.upgrade() else {
            return div().into_any_element();
        };

        let session_kind = session_host.read(cx).kind;

        // A Team member takes its requests from the room: what is typed here
        // goes to the Team that owns the room, which sends it and records the
        // exchange for every member. The card under the transcript holds what
        // the member is asking of the user, the input, and the settings its
        // next turn runs with, on the same column and card as an ordinary
        // conversation.
        if self.team_member {
            let interactions = self.render_team_interactions(window, cx);

            // A member waiting on the user's answer cannot take a request
            // until it has one, and its question is drawn just above.
            let waiting = self.session.borrow().input().waiting();

            return v_flex()
                .size_full()
                .min_h_0()
                .track_focus(&self.focus)
                .child(div().flex_1().min_h_0().child(self.transcript.clone()))
                .child(
                    transcript_column(
                        v_flex()
                            .w_full()
                            .child(
                                composer_card(cx)
                                    .debug_selector(|| "team-member-composer".into())
                                    .children(interactions)
                                    .child(
                                        composer_input_row()
                                            .capture_action(cx.listener(
                                                |this, action: &Enter, window, cx| {
                                                    match composer_enter_behavior(
                                                        cx.global::<AgentSettings>()
                                                            .newline_shortcut,
                                                        action,
                                                    ) {
                                                        ComposerEnterBehavior::InsertNewline => {
                                                            this.input.update(cx, |input, cx| {
                                                                input.replace("\n", window, cx)
                                                            })
                                                        }
                                                        ComposerEnterBehavior::Submit
                                                        | ComposerEnterBehavior::ActivateOrSubmit => {
                                                            this.send_user_message(window, cx)
                                                        }
                                                    }

                                                    cx.stop_propagation();
                                                },
                                            ))
                                            .child(div().flex_1().min_w_0().child(
                                                Textarea::new(&self.input).appearance(false),
                                            )),
                                    )
                                    .child(
                                        composer_controls_row()
                                            .child(div().flex_1().min_w_0().child(render_row(
                                                &self.session.borrow().controls,
                                                session_kind,
                                                cx,
                                            )))
                                            .child(
                                                send_button("team-member-send", false, waiting)
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.send_user_message(window, cx)
                                                        },
                                                    )),
                                            ),
                                    ),
                            )
                            .child(self.render_composer_status(cx)),
                        cx,
                    )
                    .pb_3()
                    .pt_1(),
                )
                .into_any_element();
        }

        let command_palette = self.palette_model(cx).map(|model| {
            let hover_selects = self.branch_picker_is_open();

            self.palette.render(model, hover_selects, cx)
        });

        let notices = composer_notice_panel(
            self.palette
                .render_feedback(self.session.borrow().commands(), cx)
                .map(IntoElement::into_any_element)
                .into_iter()
                .chain(
                    queued_prompts(self.session.borrow().queued_prompts(), cx)
                        .map(IntoElement::into_any_element),
                )
                .collect(),
            cx,
        );

        let side_chat =
            (self.side_chat_shown() == Some(true)).then(|| side_chat_window(&self.side_chat, cx));

        let approval = self.render_approval_panel(cx);
        let composer_free = !self.branch_flow_holds_composer();

        let questions = self
            .prompts
            .render(&self.session, composer_free, window, cx);

        let running = self.session.borrow().runtime().status() == Status::Running;

        let update_suspended = self
            .session
            .borrow()
            .runtime()
            .update_suspension()
            .is_some();

        let update_banner = update_banner(self.session.borrow().runtime().update_suspension(), cx);
        let multi_root_notice = self.render_multi_root_notice(cx);

        let update_overlay =
            update_overlay(self.session.borrow().runtime().update_suspension(), cx);

        let start_failure = self
            .session
            .borrow()
            .runtime()
            .start_failure()
            .map(str::to_owned);

        let start_overlay = start_overlay(start_failure, self.shows_start_overlay(), cx);

        // A branch settled from the backend's answer has no window to reach
        // the composer through, so the prompt it cut in front of is put back
        // here, in the frame that answer asked for.
        self.branch.fill_branch_prompt(&self.input, window, cx);

        let branch_flow_active = self.branch_flow_holds_composer();
        let branch_flow_working = self.branch_flow_is_working();
        let session_loading = self.history_ui.mode == RecentSessionsMode::Loading;

        let background = if cx
            .global::<AgentSettings>()
            .pane_background_follows_terminal
        {
            cx.global::<AgentSettings>().terminal_background
        } else {
            cx.theme().sidebar
        };

        // Blank tabs expose recent sessions automatically; `/resume` can
        // request the same list after a conversation has started. A count
        // result reserves placeholder rows until the full entries arrive.
        let transcript_empty = self.transcript.read(cx).is_empty();
        let composer_empty = self.input.read(cx).text().len() == 0;

        let history = self
            .history_ui
            .is_visible(transcript_empty, composer_empty)
            .then(|| self.history_ui.render(cx));

        // A list opened over a live conversation is a picker, and the
        // transcript behind it is not what the next click should reach. Blur
        // pushes it back a layer while keeping the tab recognizable as that
        // conversation; a blank tab has nothing to push back.
        let blur_transcript = history.is_some() && !transcript_empty;

        // Progress stays up while the history list is open: the list floats
        // over it, and hiding the panel would move the transcript under the
        // blur for a picker that is about to close again.
        let progress = {
            let session = self.session.borrow();

            self.progress_panel.render(
                session.goal(),
                session.task_list(),
                session.plan_mode(),
                window,
                cx,
            )
        };

        let now = Instant::now();

        let transcript_frost =
            self.history_ui
                .transcript_blur
                .drive(blur_transcript, now, window, cx);

        // One layer holds the pane for both the update and the start; a start
        // over an update is the more recent thing to say.
        let blocking_body = start_overlay.or(update_overlay);

        let blocking_layer = self.blocking_overlay.render(blocking_body, now, window, cx);

        v_flex()
            .size_full()
            .relative()
            // The outer frame matches the window chrome. The Agent surface owns
            // its fill so an opaque main view does not color the rounded frame.
            .bg(background.alpha(cx.global::<AgentSettings>().background_opacity))
            .rounded(UI_RADIUS - px(1.))
            .overflow_hidden()
            .track_focus(&self.focus)
            .on_prepaint(self.side_chat.track_pane())
            // Escape force-stops the agent whenever the pane or composer has
            // focus. The input propagates Escape here when the editor did not
            // consume it (inline completion, IME), and transcript clicks focus
            // the pane below. A pending approval is cancelled (deny +
            // interrupt), while a running turn is interrupted directly.
            .on_action(cx.listener(Self::on_escape))
            // The agent tab is a terminal surface stand-in, so it overrides the
            // chrome's UI font with its own configured font (Settings → Agent
            // Font), same as the terminal pane does with the terminal font.
            .font(cx.global::<AgentSettings>().font())
            .text_size(px(cx.global::<AgentSettings>().font_size))
            .children(multi_root_notice)
            .children(update_banner)
            .child(
                div()
                    .debug_selector(|| "agent-progress-transcript".into())
                    .flex_1()
                    .min_h_0()
                    // Selectable transcript text claims focus during mouse-down
                    // dispatch, so restore the composer on release. Escape then
                    // reaches the pane-level interrupt handler through the input.
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_transcript_mouse_up))
                    .relative()
                    .child(self.transcript.clone())
                    // The layer swallows clicks aimed at the transcript; the
                    // list's outside-click handler still sees them and
                    // dismisses itself.
                    .when(!transcript_frost.gone(), |this| {
                        this.child(FrostedLayer::new(transcript_frost).light())
                    }),
            )
            .child({
                // Progress takes its own space above the composer, so opening
                // its details pushes the transcript up instead of covering it.
                // Painting it before the card tucks its lower edge behind the
                // card's shadow.
                transcript_column(
                    v_flex()
                        .w_full()
                        .children(progress)
                        .children(notices)
                        .child(
                            div()
                                .w_full()
                                .relative()
                                // History is a picker over the conversation, so it
                                // floats from the card's top edge and leaves the
                                // progress panel and the transcript where they are.
                                .children(history.map(|panel| {
                                    div()
                                        .absolute()
                                        .left_0()
                                        .right_0()
                                        .bottom(relative(1.))
                                        .child(panel)
                                }))
                                .child(
                                    composer_card(cx)
                                        .debug_selector(|| "agent-progress-composer".into())
                                        .children(approval)
                                        .children(questions)
                                        .children(self.attachments.render(cx))
                                        .child(
                                            composer_input_row()
                                                // GPUI resolves these keystrokes
                                                // into Textarea actions before raw
                                                // key listeners run. Capturing
                                                // the actions lets the palette
                                                // own navigation while visible;
                                                // the handler propagates them
                                                // unchanged when it is closed.
                                                // The composer's own paste inserts
                                                // text; an image on the clipboard has
                                                // to be taken before it gets there.
                                                .capture_action(cx.listener(
                                                    |this, _: &Paste, window, cx| {
                                                        if this.paste_image(window, cx) {
                                                            cx.stop_propagation();
                                                        }
                                                    },
                                                ))
                                                .capture_action(cx.listener(
                                                    |this, _: &MoveUp, window, cx| {
                                                        this.handle_palette_control(
                                                            PaletteControl::Previous,
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                                .capture_action(cx.listener(
                                                    |this, _: &MoveDown, window, cx| {
                                                        this.handle_palette_control(
                                                            PaletteControl::Next,
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                                .capture_action(cx.listener(
                                                    |this, action: &Enter, window, cx| {
                                                        match composer_enter_behavior(
                                                    cx.global::<AgentSettings>().newline_shortcut,
                                                    action,
                                                ) {
                                                    ComposerEnterBehavior::InsertNewline => {
                                                        this.input.update(cx, |input, cx| {
                                                            input.replace("\n", window, cx);
                                                        });

                                                        cx.stop_propagation();
                                                    }
                                                    ComposerEnterBehavior::Submit => {
                                                        this.send_user_message(window, cx);

                                                        cx.stop_propagation();
                                                    }
                                                    ComposerEnterBehavior::ActivateOrSubmit => this
                                                        .handle_palette_control(
                                                            PaletteControl::Activate,
                                                            window,
                                                            cx,
                                                        ),
                                                }
                                                    },
                                                ))
                                                .capture_action(cx.listener(
                                                    |this, _: &IndentInline, window, cx| {
                                                        this.handle_palette_control(
                                                            PaletteControl::Complete,
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                                .capture_action(cx.listener(
                                                    |this, _: &Escape, window, cx| {
                                                        this.handle_palette_control(
                                                            PaletteControl::Dismiss,
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                                // The prompt editor reads larger than the
                                                // chrome around it (t3code uses 16px over
                                                // a 14px UI); +2 keeps that ratio at any
                                                // configured agent font size.
                                                .text_size(px(cx
                                                    .global::<AgentSettings>()
                                                    .font_size
                                                    + 2.0))
                                                .child(
                                                    div().flex_1().min_w_0().child(
                                                        Textarea::new(&self.input)
                                                            .appearance(false)
                                                            .disabled(
                                                                branch_flow_working
                                                                    || session_loading
                                                                    || update_suspended,
                                                            ),
                                                    ),
                                                ),
                                        )
                                        .child(
                                            composer_controls_row()
                                                .child(div().flex_1().min_w_0().child(render_row(
                                                    &self.session.borrow().controls,
                                                    session_kind,
                                                    cx,
                                                )))
                                                .children(self.render_last_response(cx))
                                                // Send stands at the card's trailing
                                                // corner, past the settings it is
                                                // qualified by: those say what the next
                                                // message is sent as, and this is the
                                                // one control that sends it, so it is
                                                // the last thing the eye reaches on its
                                                // way out of the card. Stop replaces
                                                // Send in place while a turn runs.
                                                .child(
                                                    send_button(
                                                        "agent-send",
                                                        running,
                                                        !running
                                                            && (branch_flow_active
                                                                || session_loading
                                                                || update_suspended),
                                                    )
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            if running {
                                                                this.interrupt_from_ui(window, cx)
                                                            } else {
                                                                this.send_user_message(window, cx)
                                                            }
                                                        },
                                                    )),
                                                ),
                                        ),
                                )
                                // The status footer reads out what the session has
                                // spent so far, which is context for the message
                                // rather than part of composing it. It sits under
                                // the card on the pane's own surface, so the card's
                                // edge still ends at the input it encloses.
                                .child(self.render_composer_status(cx))
                                .children(command_palette.map(|palette| {
                                    div()
                                        .absolute()
                                        .left_0()
                                        .right_0()
                                        .bottom(relative(1.))
                                        .mb_2()
                                        .occlude()
                                        .child(palette)
                                })),
                        ),
                    cx,
                )
                .pb_3()
                .pt_1()
            })
            // The Side Chat window floats over the transcript and the composer
            // but under the blocking layer, which must cover the whole pane.
            .children(side_chat)
            // Painted last so it sits over the transcript and the composer.
            .children(blocking_layer)
            .into_any_element()
    }
}

// The composer sits in the same column as the transcript above it, so the
// two edges line up at every window width.

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

fn replace_input_with_history<T: 'static>(
    input: &Entity<TextareaState>,
    text: String,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let end = text.len();

    input.update(cx, |input, cx| {
        input.set_value(text, window, cx);

        input.set_selected_range(end..end, cx);
    });
}

#[derive(Clone, Copy)]
enum PaletteControl {
    Previous,
    Next,
    Activate,
    Complete,
    Dismiss,
}

impl PaletteControl {
    /// Which way this control moves a highlighted row, or `None` where it moves
    /// none. Several lists take the same keys — the command palette, the recent
    /// conversations, the rewind and fork pickers — so the reading lives here
    /// rather than beside each list that acts on it.
    fn direction(self) -> Option<PaletteDirection> {
        match self {
            PaletteControl::Previous => Some(PaletteDirection::Previous),
            PaletteControl::Next => Some(PaletteDirection::Next),
            PaletteControl::Activate | PaletteControl::Complete | PaletteControl::Dismiss => None,
        }
    }
}

impl AgentPane {
    /// Append a source excerpt without replacing the pending request or sending it.
    pub fn append_code_reference(
        &mut self,
        reference: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |input, cx| {
            let text = input.text().to_string();

            let separator = if text.is_empty() { "" } else { "\n\n" };

            input.set_value(format!("{text}{separator}{reference}\n"), window, cx);
        });

        cx.notify();
    }
}
