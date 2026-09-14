//! The agent conversation pane: one backend session per tab, the composer,
//! the transcript, thread controls, and child-agent views. Harnesses may share
//! a process while keeping their session state isolated.
//!
//! The application shell owns tabs, chrome, provider updates, and settings;
//! this module reads only the [`settings::AgentSettings`] snapshot the shell
//! installs and exposes the pane plus the recovery types the update
//! coordinator drives across a backend replacement.

pub use crate::agent_tab::profile::{
    AgentKind, AgentKindExt, AgentThreadDefaults, agent_launch, thread_settings_from_defaults,
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

use std::borrow::Cow;

use std::cell::{Ref, RefCell};

use std::ops::Range;

use std::path::Path;

use std::rc::Rc;

use std::sync::Arc;

use std::time::{Duration, Instant};

use std::{env, fs};

use gpui::prelude::*;

use gpui::{
    AnyElement, App, AsyncApp, Bounds, ClipboardEntry, ClipboardItem, Context, Entity, FocusHandle,
    FontWeight, Hsla, Image, ImageFormat, IntoElement, ListSizingBehavior, MouseButton,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollStrategy, SharedString, WeakEntity,
    Window, div, px, relative, size,
};

use gpui_base::TextSelection;

use gpui_component::button::{Button, ButtonVariants as _};

use gpui_component::checkbox::Checkbox;

use gpui_component::dialog::{DIALOG_BUTTON_MIN_WIDTH, Dialog, DialogClose, DialogFooter};

use gpui_component::input::{
    Enter, Escape, IndentInline, InputEvent, InputState, MoveDown, MoveUp, Paste, Textarea,
    TextareaState,
};

use gpui_component::modern_menu::ModernMenu;

use gpui_component::progress::ProgressCircle;

use gpui_component::radio::Radio;

use gpui_component::scroll::Scrollbar;

use gpui_component::skeleton::Skeleton;

use gpui_component::spinner::Spinner;

use gpui_component::tooltip::Tooltip;

use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IconNamed, Sizable as _, WindowExt, h_flex,
    v_flex, v_virtual_list,
};

use nmt_agent::background_task::{BackgroundTaskKey, BackgroundTaskSnapshot};

use nmt_agent::catalog::adapter_commands;

use nmt_agent::chat::{
    ForkCheckpoint, Item as SessionItem, Question, QuestionInput, QuestionMode, QueuedPrompt,
    SessionScope, SessionSummary, SkillInfo, SkillReference, SlashCommandArguments,
    SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
};

use nmt_agent::claude_code::{sessions, stream_json};

use nmt_agent::codex::app_server;

use nmt_agent::session::ImageAttachment;

use nmt_agent::session::branch::{
    BranchError, BranchFailure, BranchUpdate, BranchView, FailureStage, FileProgress, PromptTarget,
};

use nmt_agent::session::children::ChildTranscript;

use nmt_agent::session::commands::CommandAdmission;

use nmt_agent::session::controller::{
    QuestionSubmission, SessionBranch, SessionController, SessionEffect, SessionFailure,
    SessionReady, SubmissionBlock,
};

use nmt_agent::session::delivery::{RecoverablePrompt, Submission};

use nmt_agent::session::history::{CountPublication, count_scoped_sessions, list_scoped_sessions};

use nmt_agent::session::input::{
    ApprovalOutcome, QuestionAction, QuestionCompletion, QuestionError, QuestionKey,
};

use nmt_agent::session::lifecycle::InterruptOutcome;

use nmt_agent::session::restore::{ResumeStart, SettingsSeed};

use nmt_agent::session::workflows::OpenWorkflowAgent;

#[cfg(test)]
use nmt_agent::transcript::TextField;

use nmt_agent::transcript::conversation::ConversationImage;

use nmt_agent::workflow::WorkflowRun;

use nmt_agent::{AgentEvent, AgentEventKind, AgentRoute, AgentWorkspace, MultiRootAccess, git};

use nmt_config::profile::AgentProfile;

use nmt_config::system::NewlineShortcut;

use rust_i18n::t;

use tracing::info;

use crate::agent_tab::capabilities::AgentCapabilities as _;

use crate::agent_tab::commands::{
    PaletteCatalogEntry, PaletteDirection, filter_palette_catalog, filter_skill_catalog,
    local_commands, merge_catalog, move_palette_selection, parse_skill_prefix, parse_slash_command,
    prepare_skill_selection, reconcile_skill_binding, resolve_choice, setting_value_label,
    validate_skill_binding,
};

use crate::agent_tab::composer::attachments::{
    AttachError, ComposerAttachments, MAX_ATTACHMENTS, THUMBNAIL, scratch_dir,
};

use crate::agent_tab::composer::{
    BranchFlow, CachedCatalog, CommandFeedbackKind, ComposerAction, PALETTE_MAX_HEIGHT,
    PaletteAction, PaletteModel, PaletteRow, PendingSlashCommand, RewindAction, SlashPalette,
    prompt_with_response_annotations, restored_input_after_interruption, rewind_prompt_label,
    rewind_timestamp, row_prompt_target, visible_prompt,
};

use crate::agent_tab::context_usage::{ContextUsageIndicator, cache_hit_percent};

use crate::agent_tab::execution::{
    AgentSession, ChildReader, CommandBinding, PresentationEffect, SessionOwner,
};

use crate::agent_tab::fade::{Fade, FrostedLayer};

use crate::agent_tab::input_history::{
    InputHistoryAction, InputHistoryDirection, InputHistoryNavigation, InputHistoryScope,
};

use crate::agent_tab::pane_state::TurnPresentation;

use crate::agent_tab::questions::{
    QuestionEditor, QuestionEditorState, QuestionPresentation, QuestionStatus,
};

use crate::agent_tab::session::errors::operation_error;

use crate::agent_tab::session::history::{
    FilesystemHistoryRequest, RecentSessionsMode, SessionHistoryUi,
};

use crate::agent_tab::session::prompts::PendingPrompts;

use crate::agent_tab::session::{
    Backend, Status, UpdateSuspension, directories_match, directory_label,
};

use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};

use crate::agent_tab::thread_controls::{launch_model, remember_defaults, render_row};

use crate::agent_tab::transcript::{
    LAST_RESPONSE_LIMIT, TranscriptView, last_response_label, relative_time, transcript_column,
};

use crate::agent_tab::view::composer_layout::{
    composer_card, composer_controls_row, composer_input_row,
};

use crate::agent_tab::view::session_state::session_state_badge;

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
        session_id: String,
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
}

/// Background-refreshed git branch of the pane's working directory.
#[derive(Default)]
struct GitBranchPoll {
    branch: Option<String>,
    ready: bool,
    refreshing: bool,
    generation: u64,
}

impl GitBranchPoll {
    fn invalidate(&mut self) {
        self.generation += 1;
        self.branch = None;
        self.ready = false;
        self.refreshing = false;
    }

    fn begin_refresh(&mut self) -> Option<u64> {
        if self.refreshing {
            return None;
        }

        self.refreshing = true;

        Some(self.generation)
    }

    fn complete(&mut self, generation: u64, branch: Option<String>) {
        if generation != self.generation {
            return;
        }

        self.branch = branch;
        self.ready = true;
        self.refreshing = false;
    }

    fn presentation(&self) -> (String, f32) {
        let label = self.branch.clone().unwrap_or_else(|| {
            if self.ready {
                t!("agent-git-no-branch").to_string()
            } else {
                t!("agent-git-detecting-branch").to_string()
            }
        });

        let opacity = if self.branch.is_some() { 0.72 } else { 0.48 };

        (label, opacity)
    }
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

    /// Provider state and transitions, independent of widgets and rendering.
    session: Rc<RefCell<SessionController>>,

    host: WeakEntity<AgentSession>,
    binding: CommandBinding,
    presenting_session_effect: bool,
    team_member: bool,
    #[cfg(test)]
    owned_session: Option<SessionOwner>,

    /// Interaction state for the thread controls under the composer.
    effort_drag: Option<usize>,

    /// The running turn's bookkeeping, from submission to settled output.
    turn: TurnPresentation,

    /// The approval and question cards that block a turn until answered.
    prompts: PendingPrompts,

    palette: SlashPalette,

    /// Cutting the conversation at an earlier point, by rewind or by fork.
    branch: BranchFlow,

    git_branch_poll: GitBranchPoll,

    /// Workflow runs of this session and the agent conversation the user has
    /// open. Workflow agents are not child agents, so they never reach the
    /// `Background Tasks` state above.
    workflows: WorkflowUi,

    /// Ramp of the layer that covers the pane while its backend cannot take
    /// input. Cross-fading the whole layer keeps its arrival readable as the
    /// tab being held rather than as a blur being switched on.
    overlay_fade: Fade,
}

impl AgentPane {
    /// Whether such a flow is past its picker and working. Until then the
    /// input still holds text worth editing, so only sending is refused.
    pub(crate) fn branch_flow_is_working(&self) -> bool {
        self.session.borrow().branch.is_working()
    }

    /// Whether a list of branch points is on screen, which is what makes the
    /// palette's highlight something the transcript follows.
    pub(crate) fn branch_picker_is_open(&self) -> bool {
        self.session.borrow().branch.picker_is_open()
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

        if !self.session.borrow_mut().branch.cancel_picker() {
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

        if self.session.borrow().runtime.status() != Status::Idle || self.is_command_busy() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                SharedString::from(t!("agent-fork-idle-only")),
                cx,
            );

            return false;
        }

        if let Err(error) = {
            let mut guard = self.session.borrow_mut();

            let state = &mut *guard;

            state.branch.begin_fork(&mut state.runtime, target)
        } {
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

    pub(crate) fn show_fork_checkpoints(
        &mut self,
        checkpoints: Result<Vec<ForkCheckpoint>, String>,
        cx: &mut Context<Self>,
    ) {
        let update = {
            let mut guard = self.session.borrow_mut();

            let state = &mut *guard;

            state
                .branch
                .fork_checkpoints(&mut state.runtime, checkpoints)
        };

        self.on_fork_update(update, cx);
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

    pub(crate) fn fork_palette_model(&self, state: BranchView<'_>) -> Option<PaletteModel> {
        match state {
            BranchView::LoadingFork => Some(PaletteModel {
                rows: vec![cancel_row()],
                note: Some(SharedString::from(t!("agent-fork-loading-checkpoints"))),
            }),
            BranchView::ForkCheckpoints(checkpoints) => {
                let mut rows = checkpoints
                    .iter()
                    .cloned()
                    .map(|checkpoint| PaletteRow {
                        label: rewind_prompt_label(&checkpoint.prompt).into(),
                        description: SharedString::from(t!("agent-fork-branch-before-prompt")),
                        hint: rewind_timestamp(checkpoint.timestamp.as_deref()).map(Into::into),
                        disabled_reason: None,
                        action: PaletteAction::ForkCheckpoint(checkpoint),
                    })
                    .collect::<Vec<_>>();

                rows.push(cancel_row());

                Some(PaletteModel {
                    rows,
                    note: Some(SharedString::from(t!("agent-fork-choose-prompt"))),
                })
            }
            _ => None,
        }
    }

    pub(crate) fn start_conversation_branch(
        &mut self,
        checkpoint: ForkCheckpoint,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        let update = {
            let mut guard = self.session.borrow_mut();

            let state = &mut *guard;

            state.branch.fork(&mut state.runtime, checkpoint)
        };

        self.on_fork_update(update, cx);
    }

    pub(crate) fn branch_flow_holds_composer(&self) -> bool {
        self.session.borrow().branch.holds_composer()
    }

    pub(crate) fn complete_branch(&mut self, completion: SessionBranch, cx: &mut Context<Self>) {
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

        if self.session.borrow().runtime.status() != Status::Idle || self.is_command_busy() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                SharedString::from(t!("agent-rewind-idle-only")),
                cx,
            );

            return false;
        }

        let cwd = self.cwd(cx);

        let outcome = {
            let mut guard = self.session.borrow_mut();

            let state = &mut *guard;

            state.branch.begin_rewind(&state.runtime, cwd, target)
        };

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

    pub(crate) fn rewind_palette_model(&self, state: BranchView<'_>) -> Option<PaletteModel> {
        match state {
            BranchView::LoadingRewind => Some(PaletteModel {
                rows: vec![PaletteRow {
                    label: SharedString::from(t!("agent-rewind-cancel")),
                    description: SharedString::from(t!("agent-rewind-cancel-description")),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                }],
                note: Some(SharedString::from(t!("agent-rewind-loading-active-branch"))),
            }),
            BranchView::RewindCheckpoints(checkpoints) => {
                let mut rows = checkpoints
                    .iter()
                    .cloned()
                    .map(|checkpoint| PaletteRow {
                        label: rewind_prompt_label(&checkpoint.prompt).into(),
                        description: SharedString::from(t!("agent-rewind-return-before-prompt")),
                        hint: rewind_timestamp(checkpoint.timestamp.as_deref()).map(Into::into),
                        disabled_reason: None,
                        action: PaletteAction::RewindCheckpoint(checkpoint),
                    })
                    .collect::<Vec<_>>();

                rows.push(PaletteRow {
                    label: SharedString::from(t!("agent-rewind-cancel")),
                    description: SharedString::from(t!("agent-rewind-cancel-description")),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                });

                Some(PaletteModel {
                    rows,
                    note: Some(SharedString::from(t!("agent-rewind-choose-prompt"))),
                })
            }
            BranchView::RewindAction(checkpoint, files) => {
                let file_disabled = match checkpoint.file_restore_availability {
                    sessions::FileRestoreAvailability::Unavailable => Some(SharedString::from(t!(
                        "agent-rewind-file-checkpoint-unavailable"
                    ))),
                    _ => None,
                };

                let file_description = match checkpoint.file_restore_availability {
                    sessions::FileRestoreAvailability::Available => {
                        t!("agent-rewind-files-description-available")
                    }
                    sessions::FileRestoreAvailability::Unknown => {
                        t!("agent-rewind-files-description-unknown")
                    }
                    sessions::FileRestoreAvailability::Unavailable => {
                        t!("agent-rewind-files-description-unavailable")
                    }
                };

                let files_only_disabled = match files {
                    FileProgress::Restored => {
                        Some(SharedString::from(t!("agent-rewind-files-restored")))
                    }
                    FileProgress::NotConfirmed => file_disabled.clone(),
                };

                Some(PaletteModel {
                    rows: vec![
                        PaletteRow {
                            label: SharedString::from(t!("agent-rewind-restore-files")),
                            description: SharedString::from(file_description),
                            hint: Some(SharedString::from(t!("agent-rewind-files-only"))),
                            disabled_reason: files_only_disabled,
                            action: PaletteAction::RewindAction(RewindAction::Files),
                        },
                        PaletteRow {
                            label: SharedString::from(t!("agent-rewind-restore-conversation")),
                            description: SharedString::from(t!(
                                "agent-rewind-conversation-description"
                            )),
                            hint: Some(SharedString::from(t!("agent-rewind-conversation-only"))),
                            disabled_reason: None,
                            action: PaletteAction::RewindAction(RewindAction::Conversation),
                        },
                        PaletteRow {
                            label: SharedString::from(t!(
                                "agent-rewind-restore-files-conversation"
                            )),
                            description: SharedString::from(t!(
                                "agent-rewind-combined-description"
                            )),
                            hint: Some(SharedString::from(t!("agent-rewind-combined"))),
                            disabled_reason: file_disabled,
                            action: PaletteAction::RewindAction(RewindAction::FilesAndConversation),
                        },
                        PaletteRow {
                            label: SharedString::from(t!("agent-rewind-cancel")),
                            description: SharedString::from(t!("agent-rewind-cancel-description")),
                            hint: None,
                            disabled_reason: None,
                            action: PaletteAction::RewindAction(RewindAction::Cancel),
                        },
                    ],
                    note: Some(
                        t!(
                            "agent-rewind-selected",
                            prompt = &rewind_prompt_label(&checkpoint.prompt)
                        )
                        .into_owned()
                        .into(),
                    ),
                })
            }
            _ => None,
        }
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

        let update = {
            let mut guard = self.session.borrow_mut();

            let state = &mut *guard;

            state.branch.rewind(&mut state.runtime, action)
        };

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

                self.palette.reset_discovery(false);

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
        let Some(session_host) = self.host.upgrade() else {
            return String::new();
        };

        let session_kind = session_host.read(cx).kind;

        match error {
            BranchError::Busy => t!("agent-rewind-idle-only").to_string(),
            BranchError::NotReady => t!(
                "agent-session-still-starting",
                name = session_kind.display()
            )
            .into_owned(),
            BranchError::MissingSession => t!("agent-rewind-no-session-id").to_string(),
            BranchError::FilesUnavailable => {
                t!("agent-rewind-file-checkpoint-unavailable").to_string()
            }
            BranchError::InvalidFileResult(message) => {
                message.unwrap_or_else(|| t!("agent-rewind-invalid-file-state").to_string())
            }
            BranchError::Operation(error) => operation_error(error),
            BranchError::Failed(message) => message,
        }
    }

    pub(crate) fn report_branch_failure(&mut self, failure: BranchFailure, cx: &mut Context<Self>) {
        let error = self.branch_error_message(failure.error, cx);

        let message = match (failure.stage, failure.files) {
            (FailureStage::Checkpoints | FailureStage::ProtocolFork, _) => error,
            (FailureStage::Files, _) => t!("agent-rewind-file-failed", error = &error).into_owned(),
            (FailureStage::Conversation, FileProgress::Restored) => t!(
                "agent-rewind-conversation-failed-after-files",
                error = &error
            )
            .into_owned(),
            (FailureStage::Conversation, FileProgress::NotConfirmed) => {
                t!("agent-rewind-conversation-failed", error = &error).into_owned()
            }
            (FailureStage::Startup, FileProgress::Restored) => {
                t!("agent-rewind-start-failed-after-files").to_string()
            }
            (FailureStage::Startup, FileProgress::NotConfirmed) => {
                t!("agent-rewind-start-failed").to_string()
            }
        };

        self.palette.selected = 0;

        self.palette
            .set_feedback(CommandFeedbackKind::Error, message, cx);
    }

    /// Take a pasted image into the pending message, reporting whether the
    /// paste was consumed. A paste this leaves alone falls through to the
    /// composer's own text handling, which is what a clipboard holding text
    /// should get.
    pub(crate) fn paste_image(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(session_host) = self.host.upgrade() else {
            return false;
        };

        let session_kind = session_host.read(cx).kind;

        // An image reaches the clipboard two ways: as pixels, from a capture
        // tool or a browser, and as a file, from a file manager. Both are the
        // same gesture to the person doing it.
        let Some(image) = cx
            .read_from_clipboard()
            .into_iter()
            .flat_map(|item| item.into_entries())
            .find_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image),
                ClipboardEntry::ExternalPaths(paths) => {
                    paths.paths().iter().find_map(|path| image_file(path))
                }
                ClipboardEntry::String(_) => None,
            })
        else {
            return false;
        };

        if !session_kind.caps().image_input {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!(
                    "agent-composer-images-unsupported",
                    name = session_kind.display()
                )
                .into_owned(),
                cx,
            );

            return true;
        }

        match self
            .attachments
            .attach_image(&image, &self.input, window, cx)
        {
            Ok(()) => {
                cx.notify();

                true
            }
            Err(AttachError::Full) => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    t!("agent-composer-images-full", count = MAX_ATTACHMENTS).into_owned(),
                    cx,
                );

                true
            }
            // Something on the clipboard claimed to be an image and was not.
            // Falling through lets the composer paste whatever text is there.
            Err(AttachError::Undecodable) => false,
        }
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
                .conversation
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
            .conversation
            .borrow()
            .last_response_at
            .map(|at| last_response_label(at.elapsed().as_secs()))
            .unwrap_or_default();

        let pane = cx.entity();

        window.open_dialog(cx, move |dialog, _, _| {
            Self::cache_expiry_dialog(dialog, &pane, &idle)
        });
    }

    fn cache_expiry_dialog(dialog: Dialog, pane: &Entity<Self>, idle: &str) -> Dialog {
        let pane = pane.clone();
        let idle = idle.to_string();

        dialog
            .title(t!("agent-cache-warning-title"))
            .overlay_closable(false)
            .content(move |content, _, cx| {
                content.child(
                    v_flex()
                        .gap_1()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(idle.clone())
                        .child(t!("agent-cache-warning-message")),
                )
            })
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("agent-cache-warning-send")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .label(t!("agent-cache-warning-send"))
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);

                                pane.update(cx, |pane, cx| pane.send_user_message_now(window, cx));
                            }),
                    )
                    .child(
                        DialogClose::new().child(
                            Button::new("agent-cache-warning-cancel")
                                .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                .primary()
                                .label(t!("agent-cache-warning-cancel")),
                        ),
                    ),
            )
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
                self.palette.skill_catalog.as_ref(),
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
        self.session.borrow().runtime.status() == Status::Running
            || self.session.borrow().commands.awaiting_turn
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

        let rows = self
            .history_ui
            .data
            .pending
            .unwrap_or(self.history_ui.data.sessions.len());

        if rows == 0 {
            self.history_ui.mode = RecentSessionsMode::Hidden;

            self.palette.set_feedback(
                CommandFeedbackKind::Notice,
                SharedString::from(t!("agent-composer-no-recent-sessions")),
                cx,
            );

            return true;
        }

        self.history_ui.mode = RecentSessionsMode::Open;
        self.history_ui.selected = 0;

        // A list opened from a command was opened without the pointer, and a
        // strip that was on screen the last time the pointer crossed it has
        // no way to report that the pointer has since left.
        self.history_ui.pointer_inside = false;
        self.history_ui.pointer = None;
        self.palette.feedback = None;

        cx.notify();

        true
    }

    pub(crate) fn palette_model(&mut self, cx: &Context<Self>) -> Option<PaletteModel> {
        let session_host = self.host.upgrade()?;

        let session_kind = session_host.read(cx).kind;

        match (&self.session.borrow().branch).into() {
            view @ (BranchView::LoadingRewind
            | BranchView::RewindCheckpoints(_)
            | BranchView::RewindAction(_, _)) => return self.rewind_palette_model(view),
            view @ (BranchView::LoadingFork | BranchView::ForkCheckpoints(_)) => {
                return self.fork_palette_model(view);
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

        let skills: &[SkillInfo] = if slash_skills {
            self.palette
                .skill_catalog
                .as_ref()
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
                        match self.session.borrow().runtime.status() {
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
            if slash_skills && self.palette.skill_catalog.is_none() {
                Some(SharedString::from(t!(
                    "agent-composer-skill-discovery-loading"
                )))
            } else if slash_skills
                && self
                    .palette
                    .skill_catalog
                    .as_ref()
                    .is_some_and(|catalog| !catalog.errors.is_empty())
            {
                self.palette
                    .skill_catalog
                    .as_ref()
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
        } else if session_kind.caps().async_command_discovery
            && !self.palette.provider_commands_ready
        {
            Some(SharedString::from(t!(
                "agent-composer-claude-command-loading"
            )))
        } else if slash_skills && self.palette.skill_catalog.is_none() {
            Some(SharedString::from(t!(
                "agent-composer-skill-discovery-loading"
            )))
        } else if slash_skills {
            self.palette
                .skill_catalog
                .as_ref()
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
            if self.handle_question_control(control, cx) {
                return;
            }

            if self.handle_recent_sessions_control(control, cx) {
                return;
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

    fn handle_recent_sessions_control(
        &mut self,
        control: PaletteControl,
        cx: &mut Context<Self>,
    ) -> bool {
        let composer_empty = self.input.read(cx).text().len() == 0;

        if matches!(control, PaletteControl::Complete) || !composer_empty {
            return false;
        }

        let rows = self
            .history_ui
            .data
            .pending
            .unwrap_or(self.history_ui.data.sessions.len());

        if !self.history_ui.mode.is_visible(
            self.transcript.read(cx).is_empty(),
            composer_empty,
            rows,
        ) {
            return false;
        }

        cx.stop_propagation();

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                if let Some(direction) = control.direction()
                    && let Some(selected) = move_palette_selection(
                        self.history_ui.selected,
                        self.history_ui.data.sessions.len(),
                        direction,
                    )
                {
                    self.history_ui.selected = selected;

                    self.history_ui
                        .scroll
                        .scroll_to_item(selected, ScrollStrategy::Nearest);

                    cx.notify();
                }
            }
            PaletteControl::Activate => {
                self.resume_session(self.history_ui.selected, cx);
            }
            PaletteControl::Dismiss => {
                self.history_ui.mode = RecentSessionsMode::Hidden;

                cx.notify();
            }
            // Completion belongs to the command palette. The guard above hands
            // it back before the list claims the keys, so there is nothing left
            // for it to do here.
            PaletteControl::Complete => {}
        }

        true
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
                let selected = {
                    let mut guard = self.session.borrow_mut();

                    let state = &mut *guard;

                    state
                        .branch
                        .select_checkpoint(state.runtime.epoch(), checkpoint)
                };

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

    pub(crate) fn render_command_palette(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let model = self.palette_model(cx)?;

        let selected = self
            .palette
            .selected
            .min(model.rows.len().saturating_sub(1));

        let hover_selects = self.branch_picker_is_open();

        let rows = model
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let disabled = row.disabled_reason.is_some();
                let detail = row.disabled_reason.clone().unwrap_or(row.description);
                let background = (index == selected).then(|| cx.theme().muted.opacity(0.7));

                div()
                    .id(("agent-slash-command", index))
                    .h(px(48.))
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .rounded(UI_RADIUS)
                    .when_some(background, |this, color| this.bg(color))
                    .when(disabled, |this| this.opacity(0.5))
                    .when(!disabled, |this| {
                        this.hover(|style| style.bg(cx.theme().muted.opacity(0.45)))
                    })
                    .when(hover_selects && !disabled, |this| {
                        this.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.hover_palette_index(index, cx);
                            }
                        }))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_palette_index(index, true, window, cx)
                    }))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child(row.label),
                            )
                            .children(row.hint.map(|hint| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground.opacity(0.75))
                                    .child(hint)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let note = model.note.map(|note| {
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground.opacity(0.75))
                .child(note)
        });

        Some(
            v_flex()
                .id("agent-slash-command-palette")
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_command_palette(cx)))
                .w_full()
                .max_h(PALETTE_MAX_HEIGHT)
                .overflow_y_scroll()
                .track_scroll(&self.palette.scroll)
                .p_1()
                .rounded(UI_RADIUS)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .shadow_lg()
                .children(rows)
                .children(note)
                .into_any_element(),
        )
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
        let session_profile = session_host.read(cx).profile.clone();

        if !self.binding.is_current() {
            return false;
        }

        let Some(parsed) = parse_slash_command(input) else {
            return false;
        };

        if parsed.name.is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-composer-choose-command").to_string(),
                cx,
            );

            return false;
        }

        let catalog = self.command_catalog(cx);

        let matched = catalog
            .iter()
            .find(|command| command.name == parsed.name)
            .cloned();

        let Some(command) = matched else {
            // Where a skill is invoked by writing its name into the prompt, a
            // slash line naming one is a message the harness expands, so
            // refusing it as an unknown command would block the only way to
            // reach a skill at all.
            if session_kind.caps().slash_skills_are_prompts
                && self.palette.names_a_skill(&parsed.name)
            {
                return self.send_text_inner(input.to_string(), None, None, cx);
            }

            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-composer-unknown-command", name = &parsed.name).into_owned(),
                cx,
            );

            return false;
        };

        // `/skills` owns a picker stage. A selected row rewrites the
        // composer to `$name`; the slash input itself is never a provider
        // command or an ordinary user turn.
        if command.arguments == SlashCommandArguments::Skills {
            let message = match self.palette.skill_catalog.as_ref() {
                None => t!("agent-composer-skill-discovery-loading-period").to_string(),
                Some(catalog) if catalog.skills.is_empty() && !catalog.errors.is_empty() => {
                    catalog.errors[0].clone()
                }
                Some(catalog) if catalog.skills.is_empty() => {
                    t!("agent-composer-no-skills-period").to_string()
                }
                Some(_) => t!("agent-composer-choose-skill").to_string(),
            };

            self.palette
                .set_feedback(CommandFeedbackKind::Error, message, cx);

            return false;
        }

        if command.arguments == SlashCommandArguments::None && !parsed.arguments.trim().is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-composer-command-no-arguments", name = &command.name).into_owned(),
                cx,
            );

            return false;
        }

        if command.arguments == SlashCommandArguments::Choices {
            if parsed.arguments.trim().is_empty() {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    t!("agent-composer-choose-value", name = &command.name).into_owned(),
                    cx,
                );

                return false;
            }

            let choices = self.command_choices(&command.name, cx);

            match resolve_choice(&parsed.arguments, &choices) {
                Ok(value) if command.name == "model" => {
                    self.session.borrow_mut().controls.set_model(value.clone());

                    remember_defaults(
                        &self.session.borrow().controls,
                        session_kind,
                        &session_profile,
                        cx,
                    );

                    self.palette.set_feedback(
                        CommandFeedbackKind::Notice,
                        t!("agent-composer-model-set", value = &value).into_owned(),
                        cx,
                    );

                    // Where the harness adopts a model through its own request,
                    // recording the pick is not applying it. This runs after the
                    // notice so a refusal replaces it rather than hiding under
                    // a confirmation of something that did not happen.
                    if session_kind.caps().model_selection_is_a_request {
                        self.apply_model_selection(cx);
                    }

                    return true;
                }
                Ok(value) if command.name == "permissions" => {
                    self.session.borrow_mut().controls.settings.approval = Some(value.clone());

                    remember_defaults(
                        &self.session.borrow().controls,
                        session_kind,
                        &session_profile,
                        cx,
                    );

                    self.palette.set_feedback(
                        CommandFeedbackKind::Notice,
                        t!(
                            "agent-composer-permissions-set",
                            value = &setting_value_label(&value)
                        )
                        .into_owned(),
                        cx,
                    );

                    return true;
                }
                Ok(_) => {}
                Err(message) => {
                    self.palette
                        .set_feedback(CommandFeedbackKind::Error, message, cx);

                    return false;
                }
            }
        }

        match command.name.as_str() {
            "new" | "clear" => {
                if self.is_command_busy() {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        t!("agent-composer-command-idle-only", name = &command.name).into_owned(),
                        cx,
                    );

                    false
                } else {
                    self.reset_conversation(cx);

                    true
                }
            }
            "resume" => self.open_recent_sessions(cx),
            "status" => {
                self.show_status(cx);

                true
            }
            "rewind" if session_kind.caps().file_rewind => self.open_rewind(cx),
            "rename" if session_kind.caps().session_rename => {
                self.rename_conversation(&parsed.arguments, cx)
            }
            "fork" if session_kind.caps().session_fork => self.open_fork(cx),
            // Where the conversation is a file this side rewrites, the rewind
            // picker cuts the same branch and offers restoring the files that
            // turn touched alongside it. Opening a second picker for the
            // smaller half of what one command already does would only hide
            // the choice behind the name it was reached by.
            "fork" if session_kind.caps().file_rewind => self.open_rewind(cx),
            "find" if session_kind.caps().session_search => {
                self.search_conversations(&parsed.arguments, cx)
            }
            "model" | "permissions" => false,
            _ => self.route_backend_command(
                PendingSlashCommand {
                    name: command.name,
                    arguments: parsed.arguments,
                },
                command.run_policy,
                cx,
            ),
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
                .commands
                .while_busy(command, policy);

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
            SlashCommandOutcome::Completed { message } => {
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

    pub(crate) fn run_next_queued_command(&mut self, cx: &mut Context<Self>) {
        if self.presenting_session_effect {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.advance_commands(cx));
        }
    }

    pub(super) fn show_status(&mut self, cx: &mut Context<Self>) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

        let status = match self.session.borrow().runtime.status() {
            Status::Starting => t!("agent-composer-status-starting"),
            Status::Idle => t!("agent-composer-status-idle"),
            Status::Running => t!("agent-composer-status-running"),
            Status::Exited => t!("agent-composer-status-exited"),
        };

        let mut fields = vec![
            t!(
                "agent-composer-status-field",
                name = t!("agent-composer-status-backend"),
                value = session_kind.display()
            )
            .into_owned(),
            t!(
                "agent-composer-status-field",
                name = t!("agent-composer-status-label"),
                value = status
            )
            .into_owned(),
        ];

        for (name, value) in [
            (
                t!("agent-setting-model"),
                self.session.borrow().controls.settings.model.as_deref(),
            ),
            (
                t!("agent-setting-permissions"),
                self.session.borrow().controls.settings.approval.as_deref(),
            ),
            (
                t!("agent-setting-sandbox"),
                self.session.borrow().controls.settings.sandbox.as_deref(),
            ),
            (
                t!("agent-setting-effort"),
                self.session.borrow().controls.settings.effort.as_deref(),
            ),
            (
                t!("agent-setting-tier"),
                self.session.borrow().controls.settings.tier.as_deref(),
            ),
        ] {
            if let Some(value) = value {
                fields.push(
                    t!("agent-composer-status-field", name = name, value = value).into_owned(),
                );
            }
        }

        if !self.session.borrow().commands.queue.is_empty() {
            fields.push(
                t!(
                    "agent-composer-status-field",
                    name = t!("agent-composer-status-queued"),
                    value = self.session.borrow().commands.queue.len()
                )
                .into_owned(),
            );
        }

        // Answering /status is information the user asked for, so it holds
        // rather than fading out from under them.
        self.palette
            .set_feedback(CommandFeedbackKind::Status, fields.join(" · "), cx);
    }

    pub(super) fn skill_disabled_reason(&self, skill: &SkillInfo) -> Option<SharedString> {
        if !skill.enabled {
            Some(SharedString::from(t!("agent-composer-disabled-by-codex")))
        } else {
            // A skill is invoked through the harness, so it needs a session
            // that has finished starting and has not ended.
            match self.session.borrow().runtime.status() {
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

        let language = rust_i18n::locale();

        if let Some(cached) = self
            .palette
            .catalog
            .as_ref()
            .filter(|cached| cached.language == *language)
        {
            return cached.commands.clone();
        }

        let adapter = self
            .session
            .borrow()
            .runtime
            .backend()
            .map(Backend::adapter_commands)
            .unwrap_or_else(|| adapter_commands(session_kind));

        let commands: Rc<[SlashCommandInfo]> = merge_catalog(
            local_commands(),
            adapter,
            self.palette.provider_commands.clone(),
        )
        .into();

        self.palette.catalog = Some(CachedCatalog {
            language: language.to_string(),
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
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

        self.prompts.reveal(&self.session.borrow().input, index);

        let shared = self.session.clone();
        let state = shared.borrow();
        let prompt = &state.input.batches()[index];
        let waiting = prompt.mode() != QuestionMode::Async;

        let description = prompt
            .questions()
            .first()
            .map(|question| question.question.clone())
            .unwrap_or_default();

        if waiting {
            self.emit_lifecycle(
                AgentEventKind::PermissionRequested,
                &t!("agent-session-needs-input", name = session_kind.display()),
                &description,
                cx,
            );
        }

        cx.notify();
    }

    pub(crate) fn present_question_completion(
        &mut self,
        completion: QuestionCompletion,
        cx: &mut Context<Self>,
    ) {
        if completion.started_turn {
            self.start_working(cx);

            self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
        }

        self.prompts.hide_settled(&self.session.borrow().input);

        if completion.waiting_finished {
            self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
        }

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
            .open_history(&mut self.session.borrow_mut().input, item_id, questions);

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

        if let Some(prompt) = self
            .prompts
            .questions_mut(&mut self.session.borrow_mut().input)
        {
            prompt.toggle(question, option);

            if let Some(active) = self.prompts.active
                && let Some(presentation) = self.prompts.presentations.get_mut(&active)
            {
                presentation.focus = (question, option);
            }

            cx.notify();
        }
    }

    fn handle_question_control(&mut self, control: PaletteControl, cx: &mut Context<Self>) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        if self.prompts.collapsed {
            return false;
        }

        let Some(key) = self.prompts.active else {
            return false;
        };

        let mut state = self.session.borrow_mut();

        let Some(prompt) = state.input.draft_mut(key) else {
            return false;
        };

        if prompt.mode() == QuestionMode::Async || prompt.status() != QuestionStatus::Pending {
            return false;
        }

        let Some(presentation) = self.prompts.presentations.get_mut(&key) else {
            return false;
        };

        let handled = match control {
            PaletteControl::Previous => presentation.move_focus(prompt, false),
            PaletteControl::Next => presentation.move_focus(prompt, true),
            PaletteControl::Activate => {
                let (question, option) = presentation.focus;

                if prompt
                    .questions()
                    .get(question)
                    .and_then(|question| question.options.get(option))
                    .is_none()
                {
                    return false;
                }

                prompt.toggle(question, option);

                true
            }
            PaletteControl::Complete | PaletteControl::Dismiss => false,
        };

        if handled {
            cx.stop_propagation();

            cx.notify();
        }

        handled
    }

    pub(crate) fn submit_current_questions(&mut self, cx: &mut Context<Self>) {
        let key = self
            .prompts
            .questions(&self.session.borrow().input)
            .map(|prompt| prompt.key());

        if let Some(key) = key {
            self.submit_question(key, QuestionAction::Answer, cx);
        }
    }

    pub(crate) fn skip_current_questions(&mut self, cx: &mut Context<Self>) {
        let key = self
            .prompts
            .questions(&self.session.borrow().input)
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
            QuestionSubmission::Ignored => return,
            QuestionSubmission::Settled { waiting_finished } => {
                if waiting_finished {
                    self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                }
            }
            QuestionSubmission::Waiting | QuestionSubmission::Failed => {}
        }

        self.prompts.hide_settled(&self.session.borrow().input);

        cx.notify();
    }

    pub(crate) fn prepare_question_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompts.collapsed {
            return;
        }

        let Some(batch) = self.prompts.active else {
            return;
        };

        let shared = self.session.clone();
        let state = shared.borrow();

        let Some(prompt) = state.input.draft(batch) else {
            return;
        };

        let count = prompt.questions().len();

        self.prompts
            .presentations
            .entry(batch)
            .or_insert_with(|| QuestionPresentation::new(prompt));

        drop(state);

        for index in 0..count {
            let shared = self.session.clone();
            let state = shared.borrow();

            let Some(prompt) = state.input.draft(batch) else {
                return;
            };

            let input = prompt.questions()[index].input;

            if input == QuestionInput::SelectionOnly
                || self.prompts.presentations[&batch].editors[index].is_some()
                || !prompt.pending()
            {
                continue;
            }

            let text = prompt.text(index).to_string();
            let key = prompt.key();
            let epoch = self.session.borrow().runtime.epoch();

            let on_change = move |this: &mut Self, value: String, cx: &mut Context<Self>| {
                if !this.binding.is_current() || !this.session.borrow().runtime.is_current(epoch) {
                    return;
                }

                let mut state = this.session.borrow_mut();

                let Some(prompt) = state.input.draft_mut(key) else {
                    return;
                };

                if !prompt.set_text(index, value) {
                    return;
                }

                cx.notify();
            };

            let (state, subscription) = if input == QuestionInput::Secret {
                let state = cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(true)
                        .placeholder(t!("agent-question-free-text"))
                        .default_value(text)
                });

                let subscription = cx.subscribe(&state, move |this, input, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        on_change(this, input.read(cx).value().to_string(), cx);
                    }
                });

                (QuestionEditorState::Secret(state), subscription)
            } else {
                let state = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(1, 4)
                        .placeholder(t!("agent-question-free-text"))
                        .default_value(text)
                });

                let subscription = cx.subscribe(&state, move |this, input, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        on_change(this, input.read(cx).value().to_string(), cx);
                    }
                });

                (QuestionEditorState::Text(state), subscription)
            };

            if let Some(presentation) = self.prompts.presentations.get_mut(&batch) {
                presentation.editors[index] = Some(QuestionEditor::new(state, subscription));
            }
        }
    }

    pub(crate) fn render_question_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let count = self.session.borrow().input.pending_count();

        if self.prompts.collapsed && count == 0 {
            return None;
        }

        self.prepare_question_editors(window, cx);

        let active = self.prompts.active?;
        let shared = self.session.clone();
        let state = shared.borrow();
        let prompt = self.prompts.questions(&state.input)?;
        let collapsed = self.prompts.collapsed;
        let pending = prompt.pending();

        let enabled = self
            .session
            .borrow()
            .input
            .can_submit(&self.session.borrow().runtime, prompt.key())
            && !self.branch_flow_holds_composer()
            && !self.session.borrow().commands.awaiting_turn;

        let presentation = self.prompts.presentations.get(&active)?;

        let status = match prompt.status() {
            QuestionStatus::Pending => {
                if prompt.mode() == QuestionMode::Async {
                    "agent-question-async"
                } else {
                    "agent-question-pending"
                }
            }
            QuestionStatus::Submitting => "agent-question-submitting",
            QuestionStatus::Submitted => "agent-question-submitted",
            QuestionStatus::Skipped => "agent-question-skipped",
            QuestionStatus::Expired => "agent-question-expired",
            QuestionStatus::History => "agent-question-history",
        };

        let count_label = t!("agent-question-count", count = count).into_owned();

        let mut heading = h_flex().w_full().items_center().gap_2().child(
            div()
                .flex_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(if count > 0 {
                    count_label
                } else {
                    t!(status).to_string()
                }),
        );

        let candidates: Vec<QuestionKey> = self
            .session
            .borrow()
            .input
            .batches()
            .iter()
            .filter_map(|prompt| {
                (prompt.pending() || prompt.key() == active).then_some(prompt.key())
            })
            .collect();

        if candidates.len() > 1 {
            let position = candidates
                .iter()
                .position(|index| *index == active)
                .unwrap_or(0);

            let previous = candidates[(position + candidates.len() - 1) % candidates.len()];
            let next = candidates[(position + 1) % candidates.len()];

            heading = heading
                .child(
                    Button::new("question-previous-batch")
                        .ghost()
                        .small()
                        .label("<")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.prompts.active = Some(previous);
                            this.prompts.collapsed = false;

                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .child(format!("{} / {}", position + 1, candidates.len())),
                )
                .child(
                    Button::new("question-next-batch")
                        .ghost()
                        .small()
                        .label(">")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.prompts.active = Some(next);
                            this.prompts.collapsed = false;

                            cx.notify();
                        })),
                );
        }

        heading = heading.child(
            Button::new("question-collapse")
                .ghost()
                .small()
                .label(t!(if collapsed {
                    "agent-question-open"
                } else {
                    "agent-question-collapse"
                }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.prompts.collapsed = !this.prompts.collapsed;

                    cx.notify();
                })),
        );

        let mut panel = v_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.65))
            .bg(cx.theme().muted.opacity(0.2))
            .child(heading)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if let Some(prompt) = this
                        .prompts
                        .questions_mut(&mut this.session.borrow_mut().input)
                    {
                        prompt.touch();
                    }

                    cx.notify();
                }),
            )
            .capture_key_down(cx.listener(|this, _, _, cx| {
                if let Some(prompt) = this
                    .prompts
                    .questions_mut(&mut this.session.borrow_mut().input)
                {
                    prompt.touch();
                }

                cx.notify();
            }));

        if collapsed {
            return Some(panel.into_any_element());
        }

        let mut rows = Vec::new();

        for (index, question) in prompt.questions().iter().enumerate() {
            let group: SharedString = format!("question-{active:?}-{index}").into();

            let mut row = v_flex()
                .w_full()
                .gap_1p5()
                .children(
                    question
                        .header
                        .as_ref()
                        .filter(|header| !header.is_empty())
                        .map(|header| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(header.clone())
                        }),
                )
                .child(div().text_sm().child(question.question.clone()));

            for (option_index, option) in question.options.iter().enumerate() {
                let label = option
                    .description
                    .as_ref()
                    .filter(|description| !description.is_empty())
                    .map_or_else(
                        || option.label.clone(),
                        |description| format!("{} — {description}", option.label),
                    );

                let control = if question.multi_select {
                    Checkbox::new((group.clone(), option_index))
                        .label(label)
                        .checked(prompt.is_selected(index, option_index))
                        .disabled(!enabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_question_option(index, option_index, cx)
                        }))
                        .into_any_element()
                } else {
                    Radio::new((group.clone(), option_index))
                        .label(label)
                        .checked(prompt.is_selected(index, option_index))
                        .disabled(!enabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_question_option(index, option_index, cx)
                        }))
                        .into_any_element()
                };

                row = row.child(
                    div()
                        .w_full()
                        .px_1p5()
                        .py_0p5()
                        .rounded(UI_RADIUS)
                        .when(
                            presentation.is_focused(index, option_index)
                                && prompt.mode() != QuestionMode::Async
                                && enabled,
                            |this| this.bg(cx.theme().list_active),
                        )
                        .child(control),
                );
            }

            if question.input != QuestionInput::SelectionOnly {
                if !question.options.is_empty() {
                    row = row.child(
                        Radio::new((group.clone(), question.options.len()))
                            .label(t!("agent-question-custom").into_owned())
                            .checked(prompt.is_custom(index))
                            .disabled(!enabled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(prompt) = this
                                    .prompts
                                    .questions_mut(&mut this.session.borrow_mut().input)
                                {
                                    if !prompt.choose_custom(index) {
                                        return;
                                    }

                                    if let Some(active) = this.prompts.active
                                        && let Some(presentation) =
                                            this.prompts.presentations.get(&active)
                                        && let Some(editor) = &presentation.editors[index]
                                    {
                                        editor.focus(window, cx);
                                    }

                                    cx.notify();
                                }
                            })),
                    );
                }

                if pending {
                    if let Some(editor) = &presentation.editors[index] {
                        row = row.child(editor.render(!enabled));
                    }
                } else if prompt.status() == QuestionStatus::Submitted && prompt.is_custom(index) {
                    row = row.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if question.input == QuestionInput::Secret {
                                t!("agent-question-secret-submitted").to_string()
                            } else {
                                prompt.text(index).to_string()
                            }),
                    );
                }
            }

            rows.push(row.into_any_element());
        }

        panel = panel.child(
            v_flex()
                .id(SharedString::from(format!("question-scroll-{active:?}")))
                .w_full()
                .max_h((window.viewport_size().height * 0.4).min(px(280.)))
                .overflow_y_scroll()
                .gap_3()
                .children(rows),
        );

        if let Some(error) = prompt.error() {
            let error = match error {
                QuestionError::Disconnected => t!("agent-question-disconnected").to_string(),
                QuestionError::Rejected(message) => message.clone(),
            };

            panel = panel.child(div().text_sm().text_color(cx.theme().danger).child(error));
        }

        if let Some(remaining) = prompt
            .auto_resolve_remaining(Instant::now())
            .filter(|remaining| remaining.as_secs() <= 60)
        {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        t!("agent-question-timeout", seconds = remaining.as_secs()).into_owned(),
                    ),
            );
        }

        let mut footer = h_flex().w_full().items_center().gap_2().child(
            div()
                .flex_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(t!(status)),
        );

        if pending {
            footer = footer
                .child(
                    Button::new("question-skip")
                        .ghost()
                        .disabled(!enabled)
                        .label(t!(if prompt.mode() == QuestionMode::Async {
                            "agent-question-dismiss"
                        } else {
                            "agent-question-skip"
                        }))
                        .on_click(cx.listener(|this, _, _, cx| this.skip_current_questions(cx))),
                )
                .child(
                    Button::new("question-submit")
                        .primary()
                        .disabled(!enabled || !prompt.is_complete())
                        .label(t!("agent-question-submit"))
                        .on_click(cx.listener(|this, _, _, cx| this.submit_current_questions(cx))),
                );
        }

        Some(panel.child(footer).into_any_element())
    }

    pub fn refresh_background_tasks(&mut self) {
        self.session.borrow_mut().runtime.refresh_background_tasks();
    }

    /// Provider-qualified identity of the parent session child tasks belong to.
    /// `None` until the backend reports a thread or session id, which is what
    /// disables the title-bar `Background Tasks` button.
    pub fn background_task_parent(&self) -> Option<BackgroundTaskKey> {
        self.session.borrow().runtime.background_task_parent()
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

        self.session
            .borrow_mut()
            .runtime
            .interrupt_background_task(key)
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

        let outcome = match self.session.borrow_mut().runtime.backend_mut() {
            Some(session) => session.rename_conversation(title).map_err(operation_error),
            None => Err(t!(
                "agent-session-still-starting",
                name = session_kind.display()
            )
            .into_owned()),
        };

        // The accepted title is echoed rather than the requested one: the
        // backend normalizes what it stores, and confirming text it did not
        // keep would describe a rename that did not happen that way.
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

        let mut state = self.session.borrow_mut();

        let Some(session) = state.runtime.backend_mut() else {
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
        };

        session.search_sessions(query);

        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            t!("agent-session-searching", query = query).into_owned(),
            cx,
        );

        true
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

        let removed = self
            .session
            .borrow_mut()
            .runtime
            .backend_mut()
            .is_some_and(|session| session.remove_queued_prompt(item_id));

        if !removed {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-queued-remove-failed").to_string(),
                cx,
            );

            return;
        }

        self.session.borrow_mut().delivery.removed(item_id);

        cx.notify();
    }

    pub(super) fn present_session_effect(&mut self, effect: SessionEffect, cx: &mut Context<Self>) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;
        let session_profile = session_host.read(cx).profile.clone();

        self.presenting_session_effect = true;

        self.transcript
            .update(cx, |transcript, _| transcript.sync_content());

        match effect {
            SessionEffect::Unchanged => {}
            SessionEffect::ProviderTurnAccepted { .. } => {}
            SessionEffect::TeamDecision(_) => {}
            SessionEffect::ProviderTurnFinished { .. } => {}
            SessionEffect::Changed => cx.notify(),
            SessionEffect::Title(title) => {
                self.emit_event(AgentPaneEvent::TitleSuggested(title), cx)
            }
            SessionEffect::Ready(settings) => self.on_ready(settings, cx),
            SessionEffect::Commands(commands) => {
                self.palette.provider_commands = commands;
                self.palette.catalog = None;
                self.palette.provider_commands_ready = true;
                self.palette.selected = 0;

                cx.notify();
            }
            SessionEffect::Skills(catalog) => {
                self.palette.skill_catalog = Some(catalog);
                self.palette.selected = 0;

                cx.notify();
            }
            SessionEffect::CommandResult {
                name,
                outcome,
                advance,
            } => {
                self.on_slash_command_result(&name, outcome, advance, cx);
            }
            SessionEffect::TurnStarted { opened } => self.on_turn_started(opened, cx),
            SessionEffect::TurnCompleted { error, .. } => self.on_turn_completed(error, cx),
            SessionEffect::OutputTokens(_)
            | SessionEffect::ContextWindow(_)
            | SessionEffect::ContextComposition(_)
            | SessionEffect::CompactionStarted
            | SessionEffect::CompactionFinished { .. }
            | SessionEffect::ItemStarted(_)
            | SessionEffect::ItemCompleted(_)
            | SessionEffect::TextDelta { .. }
            | SessionEffect::ConfirmedPrompts(_)
            | SessionEffect::Goal(_)
            | SessionEffect::PlanMode(_)
            | SessionEffect::Stats(_)
            | SessionEffect::StatusDetail(_) => cx.notify(),
            SessionEffect::ApprovalRequested => {
                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &t!("agent-session-needs-input", name = session_kind.display()),
                    self.session.borrow().input.approval().unwrap_or_default(),
                    cx,
                );

                cx.notify();
            }
            SessionEffect::ApprovalResolved => {
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

                cx.notify();
            }
            SessionEffect::InputRequested { index } => self.present_questions(index, cx),
            SessionEffect::InputResolved(completion) => {
                self.present_question_completion(completion, cx)
            }
            SessionEffect::Workflows { activity_changed } => {
                if activity_changed {
                    self.emit_event(AgentPaneEvent::WorkflowActivity, cx);
                }

                cx.notify();
            }
            SessionEffect::BackgroundActivity => {
                self.emit_event(AgentPaneEvent::BackgroundTaskActivity, cx);

                cx.notify();
            }
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
                remember_defaults(
                    &self.session.borrow().controls,
                    session_kind,
                    &session_profile,
                    cx,
                );

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
            }
            SessionEffect::ForkCheckpoints(checkpoints) => {
                self.show_fork_checkpoints(checkpoints, cx)
            }
            SessionEffect::HostExited { message } => self.on_host_exited(message, cx),
        }

        self.presenting_session_effect = false;
    }

    fn on_host_exited(&mut self, message: String, cx: &mut Context<Self>) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_profile = session_host.read(cx).profile.clone();

        let identity = self
            .session
            .borrow()
            .runtime
            .backend()
            .and_then(Backend::recovery_identity);

        self.session
            .borrow_mut()
            .runtime
            .reconnect(Some(RecoverySnapshot {
                identity,
                profile_name: session_profile.name.clone(),
            }));

        self.session
            .borrow_mut()
            .runtime
            .recovery_failed(message.clone());

        let failure = self.session.borrow_mut().failed(&message, true);

        self.on_error(message, true, failure, cx);
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

        let selection = ready.selection;

        self.prompts.reset_editors();

        if let Some(Err(error)) = selection {
            self.palette
                .set_feedback(CommandFeedbackKind::Error, error, cx);
        }

        info!(
            "agent thread ready: profile=\"{}\", model={:?}, profile_model={:?}",
            session_profile.name,
            self.session.borrow().controls.settings.model,
            launch_model(session_kind, &session_profile)
        );

        // The session id is known by now, so child agents that ran
        // before this tab opened can be rebuilt from history.

        cx.notify();
    }

    /// Asynchronous provider acknowledgement for a command request; feedback
    /// goes to the strip above the composer, and a settled command hands the
    /// queue to the next one.
    fn on_slash_command_result(
        &mut self,
        name: &str,
        outcome: SlashCommandOutcome,
        advance: bool,
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
            SlashCommandOutcome::Completed { message } => {
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
    fn on_turn_started(&mut self, new_turn: bool, cx: &mut Context<Self>) {
        if new_turn {
            self.start_working(cx);
        }

        self.publish_queued_user_messages(cx);

        self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);

        cx.notify();
    }

    /// Interruption is a completion state of the turn: the stop request
    /// recorded at press time becomes the transcript mark only once the
    /// backend actually ended the turn, so a backend that keeps streaming
    /// never shows an "Interrupted" row above live output. A stale request
    /// for an earlier turn is dropped at this boundary.
    fn on_turn_completed(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

        let completion_body = error
            .clone()
            .or_else(|| self.latest_agent_message(cx))
            .unwrap_or_else(|| {
                t!(
                    "agent-session-turn-completed",
                    name = session_kind.display()
                )
                .into_owned()
            });

        self.turn.refresh_timer(cx);

        self.refresh_git_branch(cx);

        self.emit_lifecycle(
            AgentEventKind::Stopped,
            &t!(
                "agent-session-provider-finished",
                name = session_kind.display()
            ),
            &completion_body,
            cx,
        );

        self.run_next_queued_command(cx);

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
                .release_secret_editors(&self.session.borrow().input);

            self.emit_event(AgentPaneEvent::Interrupted, cx);

            self.publish_queued_user_messages(cx);
        }

        if failure.cancelled_commands {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-queued-cancelled-failed").to_string(),
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
        self.history_ui.data.invalidate_filesystem_history();

        self.history_ui.data.scope = match self.history_ui.data.scope {
            SessionScope::CurrentDirectory => SessionScope::AllDirectories,
            SessionScope::AllDirectories => SessionScope::CurrentDirectory,
        };

        self.history_ui.data.sessions.clear();

        self.history_ui.data.showing_search = false;
        self.history_ui.selected = 0;

        if let Some(session) = self.session.borrow_mut().runtime.backend_mut() {
            session.request_history(self.history_ui.data.scope);
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
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

        if !session_kind.caps().filesystem_session_history {
            return;
        }

        let cwd = self.cwd(cx);
        let scope = self.history_ui.data.scope;

        let request = self
            .history_ui
            .data
            .begin_filesystem_history(cwd.clone(), self.session.borrow().runtime.epoch());

        cx.notify();

        cx.spawn(async move |this, cx| {
            Self::load_history_passes(this, request, scope, cwd, cx).await
        })
        .detach();
    }

    async fn load_history_passes(
        this: WeakEntity<Self>,
        request: FilesystemHistoryRequest,
        scope: SessionScope,
        cwd: Option<String>,
        cx: &mut AsyncApp,
    ) {
        let count_cwd = cwd.clone();

        let count = cx
            .background_executor()
            .spawn(async move { count_scoped_sessions(scope, count_cwd.as_deref()) })
            .await;

        let proceed = this
            .update(cx, |this, cx| {
                let cwd = this.cwd(cx);

                match this.history_ui.publish_filesystem_count(
                    &request,
                    cwd.as_deref(),
                    this.session.borrow().runtime.epoch(),
                    count,
                ) {
                    CountPublication::Stale => false,
                    CountPublication::Empty => {
                        cx.notify();

                        false
                    }
                    CountPublication::LoadRows => {
                        cx.notify();

                        true
                    }
                }
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
            let cwd = this.cwd(cx);

            if this.history_ui.publish_filesystem_rows(
                &request,
                cwd.as_deref(),
                this.session.borrow().runtime.epoch(),
                sessions,
            ) {
                cx.notify();
            }
        });
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
            || self.session.borrow().branch.holds_composer()
        {
            return;
        }

        let cwd = self.cwd(cx);

        let outcome = {
            let mut guard = self.session.borrow_mut();

            let state = &mut *guard;

            state
                .restore
                .begin(&mut state.runtime, session_kind, summary, cwd.as_deref())
        };

        let request = match outcome {
            ResumeStart::Busy => return,
            ResumeStart::Elsewhere { cwd, session_id } => {
                self.history_ui.selected = index;

                self.emit_event(AgentPaneEvent::ResumeElsewhere { cwd, session_id }, cx);

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

            transcript.attach_content(session.borrow().conversation.clone(), cx);

            transcript.set_owner(owner);

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
            presenting_session_effect: false,
            team_member: false,
            #[cfg(test)]
            owned_session: None,
            history_ui: SessionHistoryUi::default(),
            prompts: PendingPrompts::default(),
            effort_drag: None,
            turn: TurnPresentation::default(),
            palette: SlashPalette {
                provider_commands_ready: !kind.caps().async_command_discovery,
                ..SlashPalette::default()
            },
            branch: BranchFlow::default(),
            git_branch_poll: GitBranchPoll::default(),
            workflows: WorkflowUi::default(),
            overlay_fade: Fade::default(),
        };

        {
            let state = this.session.borrow();

            for index in 0..state.input.batches().len() {
                this.prompts.reveal(&state.input, index);
            }

            this.prompts.hide_settled(&state.input);

            if let Some(commands) = state.command_catalog() {
                this.palette.provider_commands = commands.to_vec();
                this.palette.provider_commands_ready = true;
            }

            this.palette.skill_catalog = state.skill_catalog().cloned();
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

        cx.spawn(Self::poll_git_branch).detach();

        this.load_filesystem_history(cx);

        this
    }

    async fn poll_git_branch(this: WeakEntity<Self>, cx: &mut AsyncApp) {
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
            self.git_branch_poll.invalidate();

            self.refresh_git_branch(cx);
        }

        cx.notify();
    }

    /// Append one item to the conversation, tagged with the current turn so
    /// settled turns fold as one unit.
    pub(super) fn push_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        self.push_item_with_images(item, Vec::new(), cx);
    }

    /// Append an item along with the images it carried, which only a sent
    /// user message has.
    pub(super) fn push_item_with_images(
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

        let Some(cwd) = self.cwd(cx).or_else(|| {
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

    pub fn kind(&self, cx: &App) -> Option<AgentKind> {
        Some(self.host.upgrade()?.read(cx).kind)
    }

    /// Progress through the task list this conversation is working from, as
    /// completed items out of the total, for the workspace entry's bar.
    pub fn task_tally(&self, cx: &App) -> Option<(u32, u32)> {
        self.transcript.read(cx).task_tally()
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
        let Some(session_host) = self.host.upgrade() else {
            return false;
        };

        let session_agent_route = session_host.read(cx).route.clone();
        let session_kind = session_host.read(cx).kind;

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
                .request(session_kind, title_text, tab_title_from_prompt);

        let settings = self.session.borrow().controls.settings.clone();
        let scratch = scratch_dir(session_agent_route.as_str());

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
                        bytes: image.image.bytes(),
                        media_type: image.image.format().mime_type(),
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
        if matches!(session_kind, AgentKind::Codex | AgentKind::Claude)
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
        let Some(session_host) = self.host.upgrade() else {
            return;
        };

        let session_kind = session_host.read(cx).kind;

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
            .reset_discovery(!session_kind.caps().async_command_discovery);

        self.session.borrow_mut().commands.clear();

        self.palette.feedback = None;
        self.history_ui.mode = RecentSessionsMode::Hidden;

        cx.notify();
    }

    pub(crate) fn shows_start_overlay(&self) -> bool {
        self.session.borrow().runtime.status() == Status::Starting
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

        self.palette.skill_catalog = None;
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
            .release_secret_editors(&self.session.borrow().input);

        if !self.session.borrow().branch.holds_composer() {
            self.branch.clear();
        }

        cx.notify();
    }

    /// Start the turn clock and drive the once-a-second repaint of the live
    /// progress row; the ticker stops itself once `finish_working` clears it.
    pub(crate) fn start_working(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn interrupt_from_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        let interrupted = self.session.borrow_mut().interrupt_from_user();

        if let Some((turn, prompt)) = interrupted.prompt {
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
            |host| host.read(cx).recovery_readiness(cx),
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
            Ok(()) => cx.notify(),
            Err(error) => self
                .palette
                .set_feedback(CommandFeedbackKind::Error, error, cx),
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
            Ok(()) => cx.notify(),
            Err(error) => self
                .palette
                .set_feedback(CommandFeedbackKind::Error, error, cx),
        }
    }

    pub(super) fn render_approval_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session_host = self.host.upgrade()?;

        let session_kind = session_host.read(cx).kind;

        self.session
            .borrow()
            .input
            .approval()
            .map(str::to_owned)
            .map(|approval| {
                v_flex()
                    .w_full()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border.opacity(0.65))
                    .bg(cx.theme().muted.opacity(0.2))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("agent-approval-pending")),
                    )
                    .child(
                        div()
                            // The description carries whatever the request holds:
                            // a whole plan for ExitPlanMode, a full command line
                            // for Bash. Without a ceiling the card grows past the
                            // pane and the decision buttons below it are clipped
                            // away, leaving the turn unanswerable.
                            .id("approval-description")
                            .max_h(px(256.))
                            .overflow_y_scroll()
                            .px_3()
                            .py_2()
                            .rounded(UI_RADIUS)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background.opacity(0.7))
                            .text_sm()
                            .child(approval),
                    )
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("approval-cancel")
                                    .ghost()
                                    .label(t!("agent-approval-cancel-turn"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.respond_approval("cancel", cx)
                                    })),
                            )
                            .child(
                                Button::new("approval-decline")
                                    .outline()
                                    .label(t!("agent-approval-decline"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.respond_approval("decline", cx)
                                    })),
                            )
                            // Offered only where it means something. A harness that
                            // can answer just this one call would quietly turn a
                            // session-wide grant into a single-use one.
                            .when(session_kind.caps().session_scoped_approval, |this| {
                                this.child(
                                    Button::new("approval-session")
                                        .outline()
                                        .label(t!("agent-approval-allow-session"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.respond_approval("acceptForSession", cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("approval-accept")
                                    .primary()
                                    .label(t!("agent-approval-approve-once"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.respond_approval("accept", cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            })
    }

    /// A strip naming the workspace directories the installed harness cannot
    /// use. It is not dismissible and appears before the first prompt, because
    /// a user who attached three directories would otherwise only discover the
    /// reduction from the agent failing to find a file.
    pub(super) fn render_multi_root_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session_host = self.host.upgrade()?;

        let session_kind = session_host.read(cx).kind;

        let notice = multi_root_notice(session_kind, self.configured_workspace(cx)?)?;

        Some(
            h_flex()
                .w_full()
                .px_4()
                .py_2()
                .gap_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().warning.opacity(0.10))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(notice),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_update_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.session
            .borrow()
            .runtime
            .update_suspension()
            .and_then(|state| {
                // The phases that tear the backend down and bring it back own the
                // whole surface through `render_update_overlay`, so the strip only
                // covers the two states the tab stays usable in.
                let (label, detail, failed) = match state {
                    UpdateSuspension::Waiting => (
                        t!("agent-update-waiting-label"),
                        t!("agent-update-waiting-detail"),
                        false,
                    ),
                    UpdateSuspension::Failed(message) => (
                        t!("agent-update-reconnect-failed-label"),
                        message.as_str().into(),
                        true,
                    ),
                    UpdateSuspension::Stopping
                    | UpdateSuspension::Updating
                    | UpdateSuspension::Reconnecting => return None,
                };

                let banner = h_flex()
                    .w_full()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(if failed {
                        cx.theme().danger.opacity(0.12)
                    } else {
                        cx.theme().primary.opacity(0.10)
                    })
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if failed {
                                cx.theme().danger
                            } else {
                                cx.theme().primary
                            })
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail.to_string()),
                    )
                    .when(failed, |row| {
                        row.child(
                            Button::new("agent-update-retry")
                                .outline()
                                .small()
                                .label(t!("agent-update-retry"))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.retry_update_recovery(cx)),
                                ),
                        )
                        .child(
                            Button::new("agent-update-new-session")
                                .danger()
                                .small()
                                .label(t!("agent-update-start-new-session"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_new_after_update_failure(cx)
                                })),
                        )
                    })
                    .into_any_element();

                Some(banner)
            })
    }

    /// What the blocking layer shows while the update transaction owns the
    /// backend: input would go nowhere, and the transcript underneath is a
    /// stale snapshot of a conversation that is about to be replayed.
    pub(super) fn render_update_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let label =
            update_overlay_phase(self.session.borrow().runtime.update_suspension()?)?.label();

        let body = v_flex()
            .items_center()
            .gap_3()
            .child(
                Spinner::new()
                    .icon(IconName::LoaderCircle)
                    .with_size(px(22.))
                    .color(cx.theme().primary),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(label),
            );

        Some(body.into_any_element())
    }

    /// What the blocking layer shows during the harness's start. When a start
    /// counts as still running is the session's own call.
    ///
    /// A start that failed keeps the layer and answers with the two things
    /// left to do, because the pane behind it has no conversation to return
    /// to: the transcript holds one error row and nothing else.
    pub(super) fn render_start_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let failure = self
            .session
            .borrow()
            .runtime
            .start_failure()
            .map(str::to_owned);

        if failure.is_none() && !self.shows_start_overlay() {
            return None;
        }

        let body = match &failure {
            Some(message) => v_flex()
                .max_w(px(420.))
                .items_center()
                .gap_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(message.clone()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("agent-start-retry")
                                .primary()
                                .small()
                                .label(t!("agent-start-retry"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_session(None, cx);
                                })),
                        )
                        .child(
                            Button::new("agent-start-close-tab")
                                .outline()
                                .small()
                                .label(t!("agent-start-close-tab"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.emit_event(AgentPaneEvent::CloseRequested, cx);
                                })),
                        ),
                ),
            None => v_flex()
                .items_center()
                .gap_3()
                .child(
                    ProgressCircle::new("agent-start-progress")
                        .loading(true)
                        .loading_duration(Duration::from_millis(1_200))
                        .size(px(22.))
                        .color(cx.theme().primary),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(t!("agent-start-starting")),
                ),
        };

        Some(body.into_any_element())
    }

    pub(super) fn render_composer_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let (branch, branch_opacity) = self.git_branch_poll.presentation();

        let shared = self.session.borrow().conversation.clone();
        let conversation = shared.borrow();

        let usage = conversation.context_window_usage.map(|usage| {
            ContextUsageIndicator::new(
                usage,
                conversation.context_composition.clone(),
                conversation.session_stats,
            )
        });

        // A backend that folds the count from its whole log is authoritative:
        // this side's counter sees only the turns it replayed, and a replay is
        // one page rather than the conversation.
        let turns = conversation
            .session_stats
            .map(|stats| stats.turns)
            .unwrap_or(self.session.borrow().delivery.turn());

        let stats = composer_stats_label(
            turns,
            self.transcript
                .read(cx)
                .turn_steps(self.session.borrow().delivery.turn()),
            conversation.first_output_latency,
            conversation
                .context_window_usage
                .and_then(cache_hit_percent),
        );

        h_flex()
            .w_full()
            .min_h(px(24.))
            // No rule and no fill of its own: the readouts are quiet text
            // resting on the pane, and an edge under the composer would read
            // as a second card boundary right below the card's own.
            .px(px(COMPOSER_STATUS_PADDING_X))
            .py(px(COMPOSER_STATUS_PADDING_Y))
            .items_center()
            .justify_between()
            .gap_3()
            // Everything the footer reports is an identifier or a figure — a
            // branch name, turn counts, timings, percentages — so the whole
            // strip is set in the code face rather than each readout choosing
            // for itself and the context indicator between them falling back
            // to the prose face.
            .font(cx.global::<AgentSettings>().transcript_font())
            .text_size(px(COMPOSER_STATUS_TEXT_SIZE))
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .items_center()
                    .text_color(cx.theme().muted_foreground.opacity(branch_opacity))
                    .child(Icon::new(IconName::GitBranch).size_3())
                    .child(div().min_w_0().truncate().child(branch)),
            )
            .child(
                // The readouts belong with the context indicator rather than
                // centered between it and the branch: both report what the
                // conversation has spent, and a variable-width group in the
                // middle would drift as its parts appear.
                h_flex()
                    .flex_none()
                    .gap_3()
                    .items_center()
                    .children(stats.map(|stats| {
                        div()
                            .id("agent-composer-stats")
                            .aria_label(
                                t!("agent-status-accessibility", stats = &stats).into_owned(),
                            )
                            .text_color(cx.theme().muted_foreground.opacity(0.72))
                            .child(stats)
                    }))
                    .children(usage),
            )
            .into_any_element()
    }

    /// The prompts waiting behind the running turn, one row each, above the
    /// composer. A row whose backend named it carries a control that drops it
    /// again; one this side is only remembering does not, because there is
    /// nothing on the backend such a control could address.
    pub(super) fn render_queued_prompts(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if self.session.borrow().delivery.pending().is_empty() {
            return None;
        }

        Some(
            v_flex()
                .w_full()
                .px_3()
                .py_1p5()
                .gap_0p5()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.6))
                .bg(cx.theme().muted.opacity(0.3))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .children(
                    self.session
                        .borrow()
                        .delivery
                        .pending()
                        .iter()
                        .enumerate()
                        .map(|(index, prompt)| {
                            h_flex()
                                .w_full()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(queued_message_label(prompt)),
                                )
                                .children(prompt.id.clone().map(|id| {
                                    Button::new(("queued-prompt-remove", index))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Close)
                                        .tooltip(t!("agent-history-queued-remove"))
                                        .accessibility_label(t!("agent-history-queued-remove"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.remove_queued_prompt(&id, cx)
                                        }))
                                }))
                        }),
                ),
        )
    }

    /// Height of one history row; all rows are uniform, which is what lets
    /// the virtual list precompute its scroll geometry.
    const HISTORY_ROW_HEIGHT: f32 = 28.0;

    /// Ten rows visible by default; more scroll within the fixed viewport.
    const HISTORY_MAX_HEIGHT: f32 = Self::HISTORY_ROW_HEIGHT * 10.0;

    /// The resumable-sessions block slotted into the composer shell above the
    /// input: a strip at 90% of the composer width on a slightly deeper
    /// surface, reading as a layer tucked behind the input card (t3code's
    /// context-strip look). While only the count pass has finished it shows
    /// skeleton rows at the final height, so the composer doesn't jump when
    /// the real rows land; rows render through a virtual list, so hundreds
    /// of persisted sessions cost only the visible ten.
    pub(super) fn render_history(
        &self,
        pane_background: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let rows = self
            .history_ui
            .data
            .pending
            .unwrap_or(self.history_ui.data.sessions.len());

        let body_height =
            px((Self::HISTORY_ROW_HEIGHT * rows as f32).min(Self::HISTORY_MAX_HEIGHT));

        let body: AnyElement = if self.history_ui.data.pending.is_some() {
            // Both loading and loaded bodies use the same explicit viewport
            // height. The virtual list's inferred first-frame measurement
            // must not move the composer when it replaces these placeholders.
            v_flex()
                .w_full()
                .h(body_height)
                .flex_none()
                .px_2()
                .gap_0()
                .children((0..rows.min(10)).map(|i| {
                    h_flex()
                        .h(px(Self::HISTORY_ROW_HEIGHT))
                        .w_full()
                        .px_2()
                        .items_center()
                        .child(
                            Skeleton::new()
                                .h(px(14.))
                                .w(relative(if i % 2 == 0 { 0.72 } else { 0.55 }))
                                .rounded(UI_RADIUS),
                        )
                }))
                .into_any_element()
        } else {
            let row_sizes = Rc::new(vec![size(px(0.), px(Self::HISTORY_ROW_HEIGHT)); rows]);

            div()
                .id("agent-history-rows")
                .relative()
                .w_full()
                .h(body_height)
                .flex_none()
                .overflow_hidden()
                .px_2()
                // The highlight is drawn for a pointer over the strip even
                // while a search is being typed, where the arrow keys belong
                // to the input and the keyboard has no highlight of its own.
                // The last pointer position goes with it: a pointer that left
                // and came back to the same place has moved.
                .on_hover(cx.listener(|this, inside: &bool, _, cx| {
                    if this.history_ui.pointer_inside == *inside {
                        return;
                    }

                    this.history_ui.pointer_inside = *inside;

                    if !*inside {
                        this.history_ui.pointer = None;
                    }

                    cx.notify();
                }))
                .child(
                    v_virtual_list(
                        cx.entity(),
                        "agent-history",
                        row_sizes,
                        move |this, visible_range, _, cx| {
                            // The final page in view is the cue to fetch
                            // the next one (no-op without a cursor, and
                            // only Codex pages from the backend).
                            if visible_range.end >= this.history_ui.data.sessions.len()
                                && let Some(session) =
                                    this.session.borrow_mut().runtime.backend_mut()
                            {
                                session.request_more_history();
                            }

                            visible_range
                                .map(|index| this.render_history_row(index, cx))
                                .collect()
                        },
                    )
                    .track_scroll(&self.history_ui.scroll)
                    .with_sizing_behavior(ListSizingBehavior::Infer),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.))
                        .child(Scrollbar::vertical(&self.history_ui.scroll)),
                )
                .into_any_element()
        };

        // Centered at 90% of the composer width, on the pane background
        // behind the shell: an outlined strip on a deeper tint, rounded only
        // at the top. The shell overlaps its lower edge (negative margin on
        // the shell), so the strip reads as a layer sliding out from behind
        // the front card. The extra bottom padding is clearance for that
        // overlap — without it the card would cover the last row.
        div()
            .w_full()
            .flex()
            .justify_center()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.history_ui.mode.dismisses_on_outside_click() {
                    this.history_ui.mode = RecentSessionsMode::Hidden;

                    cx.notify();
                }
            }))
            .child(
                v_flex()
                    .w(relative(0.95))
                    .rounded_t(UI_RADIUS)
                    .border_1()
                    .border_b_0()
                    .border_color(cx.theme().border.opacity(0.6))
                    // Composited over the pane rather than taken at full
                    // alpha: Fluent's `muted` is a translucent overlay tint
                    // (#00000006), so forcing its alpha to 1 would paint the
                    // strip in the tint's bare RGB - solid black in the light
                    // theme, solid white in the dark one. Blending yields the
                    // intended slightly deeper surface under either idiom,
                    // and is a no-op for themes whose `muted` is opaque.
                    .bg(pane_background.blend(cx.theme().muted))
                    .pb(px(20.))
                    .child(
                        h_flex()
                            .w_full()
                            .px_4()
                            .pt_2()
                            .pb_1()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("agent-history-recent-sessions")),
                            )
                            .child(
                                Checkbox::new("history-scope")
                                    .label(t!("agent-history-show-all-sessions").into_owned())
                                    .checked(
                                        self.history_ui.data.scope == SessionScope::AllDirectories,
                                    )
                                    .tooltip(t!("agent-history-show-all-sessions-tooltip"))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_history_scope(cx)),
                                    ),
                            ),
                    )
                    .child(body),
            )
    }

    /// The directory a listed conversation ran in, when that is not this
    /// tab's. A row from this tab's own directory says nothing by repeating
    /// it, so only the ones that will open elsewhere carry it.
    fn foreign_directory(&self, session: &SessionSummary, cx: &App) -> Option<String> {
        let cwd = session.cwd.as_deref()?;

        (!directories_match(Some(cwd), self.working_directory(cx).as_deref()))
            .then(|| directory_label(cwd))
    }

    /// One history row: title, branch, and relative time, in the settings
    /// row's ghost-control idiom (small, muted, hover lifts the foreground).
    fn render_history_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(session) = self.history_ui.data.sessions.get(index) else {
            return div().into_any_element();
        };

        // The strip's own surface is the muted tint, so a row state derived
        // from `muted` again lands on the color it sits on and disappears.
        // The list tokens are the per-theme fills meant to read against a
        // surface, translucent in Fluent and in the Modern themes alike.
        //
        // One fill, for the one current row. The pointer and the arrow keys
        // move the same highlight, so a hover tint on top of it would be a
        // second mark for a state the list only has one of.
        let selected = self.history_ui.selected == index
            && matches!(
                self.history_ui.mode,
                RecentSessionsMode::Automatic | RecentSessionsMode::Open
            )
            && (self.history_ui.pointer_inside || self.input.read(cx).text().len() == 0);

        h_flex()
            .id(("history-row", index))
            .h(px(Self::HISTORY_ROW_HEIGHT))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .rounded(UI_RADIUS)
            .cursor_pointer()
            .when(selected, |this| this.bg(cx.theme().list_active))
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.history_ui.point_at(index, event.position) {
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.resume_session(index, cx)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.5))
                            .truncate()
                            .text_sm()
                            .text_color(cx.theme().foreground.opacity(0.82))
                            .child(session.title.clone()),
                    )
                    // A search excerpt is why this row is on screen at all, so
                    // it shares the title's line rather than adding a second
                    // one that would change the list's fixed row height.
                    .children(session.snippet.clone().map(|snippet| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child(snippet.lines().collect::<Vec<_>>().join(" "))
                    })),
            )
            // Where the conversation ran, on rows that ran somewhere else.
            // Clicking one opens it there rather than continuing it here, so
            // the directory is the row's most load-bearing detail.
            .children(self.foreign_directory(session, cx).map(|directory| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .max_w(px(180.))
                    .child(
                        Icon::new(IconName::Folder)
                            .size_3()
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child(directory),
                    )
            }))
            .children(session.branch.clone().map(|branch| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .max_w(px(180.))
                    .child(
                        Icon::new(IconName::GitBranch)
                            .size_3()
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child(branch),
                    )
            }))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.55))
                    .child(relative_time(session.last_active)),
            )
            .into_any_element()
    }

    fn show_selected_text_menu(
        pane: WeakEntity<Self>,
        released_at: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let selected_text = TextSelection::selected_text(window, cx).trim().to_string();

        if selected_text.is_empty() {
            return;
        }

        // Anchored on the selection rather than the pointer, and opened above
        // it, so the text the two actions operate on stays visible while the
        // menu is up. The rect is the union of the selected line boxes, so its
        // top-left is above and left of every selected line.
        let anchor = window
            .selected_text_bounds(cx)
            .map_or(released_at, |bounds| bounds.origin);

        let copy_text = selected_text.clone();

        ModernMenu::new()
            // A selection menu offers two actions that are recognised by icon, so
            // the command row reaches them in one horizontal band instead of a
            // stack of labelled rows the pointer has to travel down.
            .commands(|menu| {
                menu.item(t!("agent-transcript-copy"), move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                })
                .icon(IconName::Copy)
                .item(t!("agent-transcript-quote"), move |window, cx| {
                    let selected_text = selected_text.clone();

                    let _ = pane.update(cx, |pane, cx| {
                        pane.add_response_annotation(selected_text, window, cx);
                    });
                })
                .icon(IconName::TextSelect)
            })
            .show_above(anchor, window, cx);
    }

    fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        // A branch or rewind picker owns Escape ahead of anything
        // under it, and closing one changes nothing else.
        if self.cancel_branch_picker(cx) {
        } else if self.session.borrow().input.approval().is_some() {
            self.respond_approval("cancel", cx);
        } else if self.prompts.questions_open(&self.session.borrow().input) {
            self.prompts.collapsed = true;

            cx.notify();
        } else if self.session.borrow().runtime.status() == Status::Running {
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
            Self::show_selected_text_menu(pane, released_at, window, cx);
        });

        cx.notify();
    }

    /// How long ago the agent last answered, beside the composer's controls.
    ///
    /// Drawn as a mark rather than as a reading: the number itself only
    /// matters once it is large enough to change what the next message costs,
    /// and until then a line of text beside the settings is one more thing to
    /// read past on the way to sending. The wording it used to carry is on the
    /// mark's tooltip, and in its accessible label.
    ///
    /// Absent until a turn has settled, and while one is running: the
    /// transcript's own live "Working for" reading is the answer then, and two
    /// clocks a few pixels apart would be read as disagreeing.
    fn render_last_response(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at = self
            .session
            .borrow()
            .conversation
            .borrow()
            .last_response_at?;

        if self.transcript.read(cx).is_working() {
            return None;
        }

        let seconds = at.elapsed().as_secs();

        let color = match last_response_tone(seconds)? {
            LastResponseTone::Warning => cx.theme().warning,
            LastResponseTone::Danger => cx.theme().danger,
        };

        let label = last_response_label(seconds);
        let tooltip = label.clone();

        Some(
            div()
                .id("agent-last-response")
                .flex_none()
                .flex()
                .items_center()
                .aria_label(label)
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .size(px(LAST_RESPONSE_MARK))
                        .text_color(color),
                )
                .into_any_element(),
        )
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
            .runtime
            .backend()
            .and_then(Backend::session_id)
            .map(str::to_owned)
    }

    /// Runs of the scoped session, in provider order.
    pub fn workflow_runs(&self) -> Ref<'_, [WorkflowRun]> {
        Ref::map(self.session.borrow(), |session| session.workflows.runs())
    }

    /// Agents of this tab the provider currently reports as running.
    pub fn running_workflow_agents(&self) -> usize {
        self.session.borrow().workflows.running_agents()
    }

    /// Rows for a skill query, shared by the `/` picker stage and the `$`
    /// prefix. Discovery runs in the background, so a missing catalog is a
    /// loading state rather than an empty result.
    fn skill_palette_model(&self, query: &str) -> PaletteModel {
        let Some(skill_catalog) = self.palette.skill_catalog.as_ref() else {
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

        if self.team_member {
            return v_flex()
                .size_full()
                .min_h_0()
                .track_focus(&self.focus)
                .child(div().flex_1().min_h_0().child(self.transcript.clone()))
                .children(self.render_approval_panel(cx))
                .children(self.render_question_panel(window, cx))
                .child(self.render_composer_status(cx))
                .into_any_element();
        }

        let command_palette = self.render_command_palette(cx);

        let command_feedback = self
            .palette
            .visible_feedback(&self.session.borrow().commands)
            .map(|feedback| {
                let (color, label) = match feedback.kind {
                    CommandFeedbackKind::Notice => {
                        (cx.theme().primary, t!("agent-feedback-notice"))
                    }
                    CommandFeedbackKind::Status => {
                        (cx.theme().muted_foreground, t!("agent-feedback-status"))
                    }
                    CommandFeedbackKind::Error => (cx.theme().danger, t!("agent-feedback-error")),
                    CommandFeedbackKind::Queued => {
                        (cx.theme().warning, t!("agent-feedback-queued"))
                    }
                };

                h_flex()
                    .w_full()
                    .gap_2()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color)
                            .child(label),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_color(cx.theme().muted_foreground)
                            .child(feedback.message.clone()),
                    )
            });

        let queued_message = self.render_queued_prompts(cx);

        let session_state = session_state_badge(
            &self.session.borrow().goal,
            self.session.borrow().plan_mode,
            cx,
        );

        let approval = self.render_approval_panel(cx);
        let questions = self.render_question_panel(window, cx);

        let action: ComposerAction = self.session.borrow().runtime.status().into();
        let running = action == ComposerAction::Stop;
        let update_suspended = self.session.borrow().runtime.update_suspension().is_some();
        let update_banner = self.render_update_banner(cx);
        let multi_root_notice = self.render_multi_root_notice(cx);
        let update_overlay = self.render_update_overlay(cx);
        let start_overlay = self.render_start_overlay(cx);

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
        let history_rows = self
            .history_ui
            .data
            .pending
            .unwrap_or(self.history_ui.data.sessions.len());

        let transcript_empty = self.transcript.read(cx).is_empty();
        let composer_empty = self.input.read(cx).text().len() == 0;

        let history = self
            .history_ui
            .mode
            .is_visible(transcript_empty, composer_empty, history_rows)
            .then(|| self.render_history(background, cx));

        // A list opened over a live conversation is a picker, and the
        // transcript behind it is not what the next click should reach. Blur
        // pushes it back a layer while keeping the tab recognizable as that
        // conversation; a blank tab has nothing to push back.
        let blur_transcript = history.is_some() && !transcript_empty;
        let now = Instant::now();

        let transcript_frost =
            self.history_ui
                .transcript_blur
                .drive(blur_transcript, now, window, cx);

        // One layer holds the pane for both the update and the start; a start
        // over an update is the more recent thing to say.
        let blocking_body = start_overlay.or(update_overlay);

        let blocking_frost = self
            .overlay_fade
            .drive(blocking_body.is_some(), now, window, cx);

        v_flex()
            .size_full()
            .relative()
            // The outer frame matches the window chrome. The Agent surface owns
            // its fill so an opaque main view does not color the rounded frame.
            .bg(background.alpha(cx.global::<AgentSettings>().background_opacity))
            .rounded(UI_RADIUS - px(1.))
            .overflow_hidden()
            .track_focus(&self.focus)
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
                // Composer area: auxiliary strips sit outside the bordered,
                // shadowed shell on a deeper surface. History is absolutely
                // anchored above the shell because it only exists while the
                // transcript is empty; loading it must never participate in
                // composer height calculation. Both strips are painted before
                // the shell, whose edge and shadow keep them visibly tucked
                // behind the input card.
                transcript_column(
                    div()
                        .w_full()
                        .relative()
                        .children(history.map(|history| {
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(relative(1.))
                                .mb(px(-14.))
                                .child(history)
                        }))
                        .child(
                            composer_card(cx)
                                .children(approval)
                                .children(questions)
                                .children(command_feedback)
                                .children(session_state)
                                .children(queued_message)
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
                                        .text_size(px(cx.global::<AgentSettings>().font_size + 2.0))
                                        .child(div().flex_1().min_w_0().child(
                                            Textarea::new(&self.input).appearance(false).disabled(
                                                branch_flow_working
                                                    || session_loading
                                                    || update_suspended,
                                            ),
                                        )),
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
                                        .child(if running {
                                            Button::new("agent-send")
                                                .primary()
                                                .size(px(COMPOSER_SEND_BUTTON))
                                                .rounded_full()
                                                .icon(StopResponseIcon)
                                                .tooltip(t!("agent-action-stop-response"))
                                                .accessibility_label(t!(
                                                    "agent-action-stop-response",
                                                ))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.interrupt_from_ui(window, cx)
                                                }))
                                        } else {
                                            Button::new("agent-send")
                                                .primary()
                                                .disabled(
                                                    branch_flow_active
                                                        || session_loading
                                                        || update_suspended,
                                                )
                                                .size(px(COMPOSER_SEND_BUTTON))
                                                .rounded_full()
                                                .icon(IconName::ArrowUp)
                                                .tooltip(t!("agent-action-send-message"))
                                                .accessibility_label(t!(
                                                    "agent-action-send-message",
                                                ))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.send_user_message(window, cx)
                                                }))
                                        }),
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
                    cx,
                )
                .pb_3()
                .pt_1()
            })
            // Painted last so it sits over the transcript and the composer.
            // Once the state it showed has ended the layer keeps fading with
            // nothing on it; the body belonged to that state.
            .when(!blocking_frost.gone(), |this| {
                this.child(
                    FrostedLayer::new(blocking_frost)
                        .padded()
                        .children(blocking_body),
                )
            })
            .into_any_element()
    }
}

// The composer sits in the same column as the transcript above it, so the
// two edges line up at every window width.

/// Diameter of the send/stop control that closes the input line.
const COMPOSER_SEND_BUTTON: f32 = 32.0;

/// The status footer along the bottom edge of the composer card. It reports
/// rather than invites input, so it is set below the chrome size to keep the
/// prompt above it the loudest thing on the card.
pub(super) const COMPOSER_STATUS_PADDING_X: f32 = 14.0;

pub(super) const COMPOSER_STATUS_PADDING_Y: f32 = 6.0;
pub(super) const COMPOSER_STATUS_TEXT_SIZE: f32 = 11.5;

pub(super) struct StopResponseIcon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ComposerEnterBehavior {
    InsertNewline,
    Submit,
    ActivateOrSubmit,
}

pub(super) fn composer_enter_behavior(
    shortcut: NewlineShortcut,
    action: &Enter,
) -> ComposerEnterBehavior {
    match (action.secondary, action.shift) {
        (false, false) => ComposerEnterBehavior::ActivateOrSubmit,
        (true, false) if shortcut == NewlineShortcut::CtrlEnter => {
            ComposerEnterBehavior::InsertNewline
        }
        (false, true) if shortcut == NewlineShortcut::ShiftEnter => {
            ComposerEnterBehavior::InsertNewline
        }
        _ => ComposerEnterBehavior::Submit,
    }
}

impl IconNamed for StopResponseIcon {
    fn path(self) -> SharedString {
        "icons/stop.svg".into()
    }
}

/// Edge of the mark. Set to the size of a settings pill's own glyph, so the
/// row it stands in keeps one glyph size across its whole width.
const LAST_RESPONSE_MARK: f32 = 12.0;

/// How far into the window a conversation has to have drifted before the
/// composer says so, and before it says so in the danger colour. The window is
/// the one a provider's prompt cache is expected to hold, so the first mark
/// says the next message is going to start costing more than the last one did,
/// and the second says it is about to cost a full re-read of the context.
const LAST_RESPONSE_WARNING: f32 = 0.5;

const LAST_RESPONSE_DANGER: f32 = 0.9;

/// How loudly the composer marks a conversation that has been sitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastResponseTone {
    Warning,
    Danger,
}

/// The mark a settled conversation carries, from how long it has been sitting.
///
/// Under half the window there is nothing worth saying: a conversation picked
/// up that soon costs what it would have cost immediately, and a reading that
/// is always on screen is one the eye stops seeing. Past the window the answer
/// stops changing, which is the same answer as the last reading inside it.
fn last_response_tone(seconds: u64) -> Option<LastResponseTone> {
    let drift = seconds as f32 / LAST_RESPONSE_LIMIT.as_secs() as f32;

    if drift >= LAST_RESPONSE_DANGER {
        Some(LastResponseTone::Danger)
    } else if drift >= LAST_RESPONSE_WARNING {
        Some(LastResponseTone::Warning)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdateOverlayPhase {
    Stopping,
    Updating,
    Reconnecting,
}

impl UpdateOverlayPhase {
    fn label(self) -> Cow<'static, str> {
        match self {
            Self::Stopping => t!("agent-update-stopping-label"),
            Self::Updating => t!("agent-update-updating-label"),
            Self::Reconnecting => t!("agent-update-reconnecting-label"),
        }
    }
}

/// The composer's one-line account of the conversation: how many turns it has
/// run, how many actions the newest turn took, how long that turn waited for
/// its first output, and how much of the input the provider had cached. Each
/// part is dropped rather than shown as a zero when nothing reports it, and a
/// conversation that has not run a turn yet reports nothing at all.
pub(super) fn composer_stats_label(
    turns: u64,
    steps: usize,
    first_output: Option<Duration>,
    cache_hit: Option<u64>,
) -> Option<String> {
    if turns == 0 {
        return None;
    }

    let mut parts = vec![t!("agent-status-turns", count = turns).into_owned()];

    if steps > 0 {
        parts.push(t!("agent-status-steps", count = steps).into_owned());
    }

    if let Some(first_output) = first_output {
        parts.push(
            t!(
                "agent-status-first-output",
                value = &latency_readout(first_output)
            )
            .into_owned(),
        );
    }

    if let Some(percent) = cache_hit {
        parts.push(t!("agent-status-cache-hit", percent = percent).into_owned());
    }

    Some(parts.join(" · "))
}

/// Sub-second latencies are the interesting ones, and a reading like `0.8s`
/// hides how much of a second it was; past a second the tenth is enough.
fn latency_readout(latency: Duration) -> String {
    if latency < Duration::from_secs(1) {
        format!("{}ms", latency.as_millis())
    } else {
        format!("{:.1}s", latency.as_secs_f64())
    }
}

pub(super) fn update_overlay_phase(state: &UpdateSuspension) -> Option<UpdateOverlayPhase> {
    match state {
        UpdateSuspension::Stopping => Some(UpdateOverlayPhase::Stopping),
        UpdateSuspension::Updating => Some(UpdateOverlayPhase::Updating),
        UpdateSuspension::Reconnecting => Some(UpdateOverlayPhase::Reconnecting),
        UpdateSuspension::Waiting | UpdateSuspension::Failed(_) => None,
    }
}

/// What an Agent Tab has to disclose about the directories its harness cannot
/// reach, or `None` when there is nothing to disclose. Derived from the
/// harness's declared access and the workspace alone, so a permission-preset
/// change can neither raise nor clear it: choosing a broader preset widens what
/// the harness may do inside the one root it has, and does not give it
/// selected-root isolation across the others.
pub(super) fn multi_root_notice(kind: AgentKind, workspace: &AgentWorkspace) -> Option<String> {
    if kind.caps().multi_root_access == MultiRootAccess::Full || !workspace.is_multi_root() {
        return None;
    }

    Some(
        t!(
            "agent-multi-root-primary-only",
            agent = kind.display(),
            path = workspace.primary().unwrap_or_default(),
            count = workspace.additional().len()
        )
        .into_owned(),
    )
}

/// One queued prompt on one line. A prompt spanning several lines is folded
/// into one so every waiting row costs the composer the same height.
pub(super) fn queued_message_label(prompt: &QueuedPrompt) -> String {
    let text = visible_prompt(&prompt.text)
        .lines()
        .collect::<Vec<_>>()
        .join(" ");

    t!("agent-history-queued-message", text = &text).into_owned()
}

fn cancel_row() -> PaletteRow {
    PaletteRow {
        label: SharedString::from(t!("agent-fork-cancel")),
        description: SharedString::from(t!("agent-fork-cancel-description")),
        hint: None,
        disabled_reason: None,
        action: PaletteAction::ForkCancel,
    }
}

/// The pane's branch label: a detached `HEAD` shows its short commit,
/// matching the git footer's presentation of the same state.
fn branch_label(cwd: &str, max_age: Duration) -> Option<String> {
    let branch = match git::current_branch(cwd, max_age) {
        Ok(branch) => branch?,
        Err(_) => return Some("Git unavailable".into()),
    };

    Some(match branch {
        git::CheckedOut::Branch(branch) => branch,
        git::CheckedOut::Detached(commit) => {
            t!("git-status-detached", commit = &commit).into_owned()
        }
    })
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

/// A copied file read as an image, or `None` for anything that is not one.
/// Only the extension is trusted to decide whether reading is worth it; the
/// decode decides whether it was an image.
fn image_file(path: &Path) -> Option<Image> {
    let format = match path
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "gif" => ImageFormat::Gif,
        "bmp" => ImageFormat::Bmp,
        _ => return None,
    };

    Some(Image::from_bytes(format, fs::read(path).ok()?))
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
