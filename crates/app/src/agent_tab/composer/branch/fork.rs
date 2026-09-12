//! Branching a conversation in front of a chosen prompt, for the backends
//! whose history lives behind their connection.
//!
//! Claude's history is a file this side reads and rewrites itself, and its
//! rewind picker offers the same cut alongside restoring the files that turn
//! touched; `/fork` opens that picker there rather than a second one that
//! would do less. What is left here is the shape the other two share: ask the
//! backend which prompts it can branch in front of, show them, and hand the
//! chosen one back so the branch starts where it was cut.

use gpui::{Context, Window};
use nmt_agent::chat::ForkCheckpoint;
pub(in crate::agent_tab) use nmt_agent::session::branch::PromptTarget;
#[cfg(test)]
pub(in crate::agent_tab) use nmt_agent::session::branch::checkpoint_at_depth;
use nmt_agent::session::branch::{BranchError, BranchUpdate, BranchView};

use crate::agent_tab::composer::branch::rewind::{rewind_prompt_label, rewind_timestamp};
use crate::agent_tab::composer::{CommandFeedbackKind, PaletteAction, PaletteModel, PaletteRow};
use crate::agent_tab::session::Status;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::{AgentPane, RecentSessionsMode, translated};

/// Name the prompt one picker row stands for.
///
/// Both pickers list their branch points newest first and append their cancel
/// row last, which is the same order `depth` counts in, so a row's own index
/// is the depth of the prompt it offers. The text travels with it, so the
/// transcript can refuse to move if the two lists have drifted apart.
pub(in crate::agent_tab) fn row_prompt_target(
    row: usize,
    action: &PaletteAction,
) -> Option<PromptTarget> {
    let prompt = match action {
        PaletteAction::RewindCheckpoint(checkpoint) => checkpoint.prompt.clone(),
        PaletteAction::ForkCheckpoint(checkpoint) => checkpoint.prompt.clone(),
        _ => return None,
    };

    Some(PromptTarget { prompt, depth: row })
}

impl AgentPane {
    /// Whether such a flow is past its picker and working. Until then the
    /// input still holds text worth editing, so only sending is refused.
    pub(in crate::agent_tab) fn branch_flow_is_working(&self) -> bool {
        self.session.borrow().branch.is_working()
    }

    /// Whether a list of branch points is on screen, which is what makes the
    /// palette's highlight something the transcript follows.
    pub(in crate::agent_tab) fn branch_picker_is_open(&self) -> bool {
        self.session.borrow().branch.picker_is_open()
    }

    /// Hand the transcript to a picker that is about to scroll it to the
    /// prompt it highlights.
    pub(in crate::agent_tab) fn hold_transcript_for_picker(&self, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |transcript, _| transcript.hold_for_picker());
    }

    /// Give it back, for a picker closing without having cut anything.
    pub(in crate::agent_tab) fn release_transcript_from_picker(&self, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |transcript, cx| transcript.release_from_picker(cx));
    }

    /// Move the transcript to the prompt the highlighted picker row names, so
    /// the conversation shows what the cut would keep and what it would drop.
    /// Following the smooth-scrolling setting keeps the jump between two
    /// distant prompts readable where the user asked for animated scrolling.
    pub(in crate::agent_tab) fn follow_branch_selection(&mut self, cx: &mut Context<Self>) {
        let selected = self.palette.selected;

        let Some(target) = self
            .palette_model(cx)
            .and_then(|model| model.rows.get(selected).cloned())
            .and_then(|row| row_prompt_target(selected, &row.action))
        else {
            return;
        };

        let smooth = cx.global::<AgentSettings>().smooth_wheel;

        self.transcript.update(cx, |transcript, cx| {
            transcript.scroll_to_prompt(&target, smooth, cx)
        });
    }

    pub(in crate::agent_tab) fn cancel_branch_picker(&mut self, cx: &mut Context<Self>) -> bool {
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

    pub(in crate::agent_tab) fn fork_from_prompt(
        &mut self,
        target: PromptTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        self.request_fork_checkpoints(Some(target), cx)
    }

    pub(in crate::agent_tab) fn open_fork(&mut self, cx: &mut Context<Self>) -> bool {
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
                translated("agent-fork-idle-only"),
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
                BranchError::Busy => translated("agent-fork-idle-only"),
                _ => self.branch_error_message(error).into(),
            };

            self.palette
                .set_feedback(CommandFeedbackKind::Error, message, cx);

            return false;
        }

        self.branch.pending_prompt = None;
        self.palette.selected = 0;
        self.palette.dismissed = false;

        self.palette.set_feedback(
            CommandFeedbackKind::Status,
            translated("agent-fork-loading-checkpoints"),
            cx,
        );

        true
    }

    pub(in crate::agent_tab) fn show_fork_checkpoints(
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

        self.apply_fork_update(update, cx);
    }

    pub(in crate::agent_tab) fn apply_fork_update(
        &mut self,
        update: BranchUpdate,
        cx: &mut Context<Self>,
    ) {
        match update {
            BranchUpdate::Empty => self.palette.set_feedback(
                CommandFeedbackKind::Error,
                translated("agent-fork-no-prompts"),
                cx,
            ),

            BranchUpdate::Picker { unresolved } => {
                self.palette.feedback = None;
                self.palette.selected = 0;

                if unresolved {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        translated("agent-fork-prompt-not-a-branch-point"),
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
                self.session.borrow_mut().restore.cancel();
                self.session.borrow_mut().controls.seed_thread_defaults = false;
                self.session.borrow_mut().controls.seed_approval_reviewer = false;

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    translated("agent-session-forking"),
                    cx,
                );

                cx.notify();
            }

            BranchUpdate::Failed(failure) => {
                let message = self.branch_error_message(failure.error);

                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
            }

            _ => {}
        }
    }

    pub(in crate::agent_tab) fn cancel_fork_picker(&mut self, cx: &mut Context<Self>) {
        self.cancel_branch_picker(cx);
    }

    pub(in crate::agent_tab) fn fork_palette_model(
        &self,
        state: BranchView<'_>,
    ) -> Option<PaletteModel> {
        match state {
            BranchView::LoadingFork => Some(PaletteModel {
                rows: vec![cancel_row()],
                note: Some(translated("agent-fork-loading-checkpoints")),
            }),

            BranchView::ForkCheckpoints(checkpoints) => {
                let mut rows = checkpoints
                    .iter()
                    .cloned()
                    .map(|checkpoint| PaletteRow {
                        label: rewind_prompt_label(&checkpoint.prompt).into(),
                        description: translated("agent-fork-branch-before-prompt"),
                        hint: rewind_timestamp(checkpoint.timestamp.as_deref()).map(Into::into),
                        disabled_reason: None,
                        action: PaletteAction::ForkCheckpoint(checkpoint),
                    })
                    .collect::<Vec<_>>();

                rows.push(cancel_row());

                Some(PaletteModel {
                    rows,
                    note: Some(translated("agent-fork-choose-prompt")),
                })
            }

            _ => None,
        }
    }

    pub(in crate::agent_tab) fn start_conversation_branch(
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

        self.apply_fork_update(update, cx);
    }

    /// Ready may arrive without a window. The next render applies the prompt
    /// only if the editor still contains the draft captured for this operation.
    pub(in crate::agent_tab) fn fill_branch_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.branch.pending_prompt.take() else {
            return;
        };

        if *self.input.read(cx).text() != pending.expected_draft {
            return;
        }

        self.input.update(cx, |input, cx| {
            let end = pending.prompt.len();

            input.set_value(pending.prompt, window, cx);
            input.set_selected_range(end..end, cx);
        });
    }
}

fn cancel_row() -> PaletteRow {
    PaletteRow {
        label: translated("agent-fork-cancel"),
        description: translated("agent-fork-cancel-description"),
        hint: None,
        disabled_reason: None,
        action: PaletteAction::ForkCancel,
    }
}
