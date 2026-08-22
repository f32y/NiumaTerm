//! Branching a conversation in front of a chosen prompt, for the backends
//! whose history lives behind their connection.
//!
//! Claude's history is a file this side reads and rewrites itself, and its
//! rewind picker offers the same cut alongside restoring the files that turn
//! touched; `/fork` opens that picker there rather than a second one that
//! would do less. What is left here is the shape the other two share: ask the
//! backend which prompts it can branch in front of, show them, and hand the
//! chosen one back so the branch starts where it was cut.

use nmt_i18n::i18n;

use crate::agent::composer::rewind::{
    rewind_blocks_submission, rewind_prompt_label, rewind_timestamp,
};
use crate::agent::composer::{CommandFeedbackKind, PaletteAction, PaletteModel, PaletteRow};
use crate::agent::*;

/// One prompt the user pointed at in the transcript, to be found in the list
/// of branch points once the backend answers with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::agent) struct PromptTarget {
    pub(in crate::agent) prompt: String,
    /// How many turn-opening prompts follow it, which is what locates it from
    /// the newest end of that list.
    pub(in crate::agent) depth: usize,
}

/// Find the branch point a pointed-at prompt names.
///
/// Both the transcript and the backend's list enumerate the prompts that
/// opened a turn, so the prompt is found by counting back from the newest one:
/// that is the end the two agree on even where an older stretch of the
/// conversation was rewritten by a compaction, or where the backend declines
/// to offer a cut it cannot make. The text is compared as well, because a
/// count landing on a different prompt means the two lists disagree — and
/// answering `None` then sends the user to the picker rather than cutting
/// somewhere they did not point at.
pub(in crate::agent) fn checkpoint_at_depth<'a, T>(
    checkpoints: &'a [T],
    target: &PromptTarget,
    prompt_of: impl Fn(&T) -> &str,
) -> Option<&'a T> {
    checkpoints
        .get(target.depth)
        .filter(|checkpoint| prompt_of(checkpoint) == target.prompt)
}

/// A branch is a local multi-step operation rather than a model turn, so its
/// progress is tracked apart from the turn state; timers, transcript rows, and
/// the slash queue must not read cutting a branch as provider output.
///
/// One request is outstanding at a time — the composer is held while the
/// picker is up, so a second `/fork` cannot be sent — which is why the answer
/// needs no request identity to be matched against.
pub(in crate::agent) enum ForkState {
    /// Waiting on the backend's list of branch points. A target means the user
    /// pointed at one prompt rather than asking to be shown the list, so the
    /// answer is resolved against it instead of opening the picker.
    Loading(Option<PromptTarget>),
    Selecting(Vec<ForkCheckpoint>),
    /// The branch was requested; the tab is waiting on the copy's own replay.
    Branching,
}

impl ForkState {
    /// Whether this state is showing rows, which is what decides if Esc has a
    /// picker to close and if the composer is blocked behind one.
    pub(in crate::agent) fn is_picker(&self) -> bool {
        matches!(self, Self::Loading(_) | Self::Selecting(_))
    }
}

#[derive(Default)]
pub(in crate::agent) struct ForkFlow {
    pub(in crate::agent) state: Option<ForkState>,
    /// The prompt the branch was cut in front of, waiting for a frame to put
    /// it back in the composer. A branch can be settled from the backend's
    /// answer, which arrives with no window to reach the input through, so the
    /// text is parked here and the next render spends it.
    pub(in crate::agent) pending_prompt: Option<String>,
}

impl AgentPane {
    /// Whether a branch or a rewind is holding the composer.
    ///
    /// Both replace the conversation under it, so a prompt sent while either
    /// is open would reach a session about to be swapped out — and while a
    /// picker is showing, the keys that would send it are the picker's.
    pub(in crate::agent) fn branch_flow_holds_composer(&self) -> bool {
        rewind_blocks_submission(self.rewind.state.as_ref()) || self.fork.state.is_some()
    }

    /// Whether such a flow is past its picker and working. Until then the
    /// input still holds text worth editing, so only sending is refused.
    pub(in crate::agent) fn branch_flow_is_working(&self) -> bool {
        self.rewind
            .state
            .as_ref()
            .is_some_and(|state| !state.is_picker())
            || self
                .fork
                .state
                .as_ref()
                .is_some_and(|state| !state.is_picker())
    }

    /// Close whichever picker is open, answering whether there was one. Escape
    /// reaches both, and only one can be open at a time.
    pub(in crate::agent) fn cancel_branch_picker(&mut self, cx: &mut Context<Self>) -> bool {
        if self
            .rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.cancel_rewind_picker(cx);
            return true;
        }
        if self.fork.state.as_ref().is_some_and(ForkState::is_picker) {
            self.cancel_fork_picker(cx);
            return true;
        }
        false
    }

    /// Branch in front of one prompt the user pointed at in the transcript.
    ///
    /// The branch points still have to be asked for — only the backend can
    /// name a cut — so this is the same request the picker makes, carrying
    /// which of the answers to act on. A target the answer does not confirm
    /// falls back to the picker rather than cutting somewhere else.
    pub(in crate::agent) fn fork_from_prompt(
        &mut self,
        target: PromptTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        self.request_fork_checkpoints(Some(target), cx)
    }

    /// Ask the backend which prompts this conversation can be branched in
    /// front of, and open the picker on the answer.
    pub(in crate::agent) fn open_fork(&mut self, cx: &mut Context<Self>) -> bool {
        self.request_fork_checkpoints(None, cx)
    }

    fn request_fork_checkpoints(
        &mut self,
        target: Option<PromptTarget>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.status != Status::Idle || self.is_command_busy() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-fork-idle-only").to_string(),
                cx,
            );
            return false;
        }

        let asked = self
            .session
            .as_mut()
            .is_some_and(Backend::request_fork_checkpoints);
        if !asked {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-still-starting").replace("{name}", self.kind.display()),
                cx,
            );
            return false;
        }

        self.fork.state = Some(ForkState::Loading(target));
        self.palette.selected = 0;
        self.palette.dismissed = false;
        self.set_command_feedback(
            CommandFeedbackKind::Status,
            i18n("agent-fork-loading-checkpoints").to_string(),
            cx,
        );
        true
    }

    /// Take the backend's answer to the open request.
    ///
    /// An answer to a request the user has already cancelled is dropped: it
    /// would otherwise reopen a picker they closed, and a second `/fork` has
    /// its own answer coming.
    pub(in crate::agent) fn show_fork_checkpoints(
        &mut self,
        checkpoints: Result<Vec<ForkCheckpoint>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(ForkState::Loading(target)) = self.fork.state.take() else {
            return;
        };

        match checkpoints {
            Ok(checkpoints) if checkpoints.is_empty() => {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-fork-no-prompts").to_string(),
                    cx,
                );
            }
            Ok(checkpoints) => {
                let pointed_at = target.as_ref().and_then(|target| {
                    checkpoint_at_depth(&checkpoints, target, |checkpoint| &checkpoint.prompt)
                        .cloned()
                });
                self.fork.state = Some(ForkState::Selecting(checkpoints));
                self.palette.feedback = None;
                self.palette.selected = 0;

                match pointed_at {
                    Some(checkpoint) => self.start_conversation_branch(checkpoint, cx),
                    None => {
                        // A pointed-at prompt the answer did not confirm is
                        // reported, because the picker opening on its own after
                        // a menu pick would otherwise look like the pick was
                        // simply the wrong one.
                        if target.is_some() {
                            self.set_command_feedback(
                                CommandFeedbackKind::Error,
                                i18n("agent-fork-prompt-not-a-branch-point").to_string(),
                                cx,
                            );
                        }
                        cx.notify();
                    }
                }
            }
            Err(message) => {
                self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
            }
        }
    }

    pub(in crate::agent) fn cancel_fork_picker(&mut self, cx: &mut Context<Self>) {
        if self.fork.state.as_ref().is_some_and(ForkState::is_picker) {
            self.fork.state = None;
            self.palette.selected = 0;
            // Cancelling is the user's own no-op, and dropping the message
            // retires the non-transient "Reading branch points…" status that
            // would otherwise outlive the picker it described.
            self.palette.feedback = None;
            cx.notify();
        }
    }

    pub(in crate::agent) fn fork_palette_model(&self, state: &ForkState) -> Option<PaletteModel> {
        match state {
            ForkState::Loading(_) => Some(PaletteModel {
                rows: vec![cancel_row()],
                note: Some(i18n("agent-fork-loading-checkpoints").to_string()),
            }),
            ForkState::Selecting(checkpoints) => {
                let mut rows = checkpoints
                    .iter()
                    .cloned()
                    .map(|checkpoint| PaletteRow {
                        label: rewind_prompt_label(&checkpoint.prompt),
                        description: i18n("agent-fork-branch-before-prompt").to_string(),
                        hint: rewind_timestamp(checkpoint.timestamp.as_deref()),
                        disabled_reason: None,
                        action: PaletteAction::ForkCheckpoint(checkpoint),
                    })
                    .collect::<Vec<_>>();
                rows.push(cancel_row());

                Some(PaletteModel {
                    rows,
                    note: Some(i18n("agent-fork-choose-prompt").to_string()),
                })
            }
            ForkState::Branching => None,
        }
    }

    /// Branch in front of the chosen prompt and move the tab into the copy.
    ///
    /// The tab moves the same way it moves into a resumed conversation, so the
    /// presentation waits on the branch's own replay: what is visible stays
    /// the conversation it branched from until the copy's history arrives to
    /// replace it, and a refused branch leaves the tab exactly where it was.
    pub(in crate::agent) fn start_conversation_branch(
        &mut self,
        checkpoint: ForkCheckpoint,
        cx: &mut Context<Self>,
    ) {
        let outcome = match self.session.as_mut() {
            Some(session) => session.fork_conversation(&checkpoint.anchor),
            None => {
                Err(i18n("agent-session-still-starting").replace("{name}", self.kind.display()))
            }
        };

        // A refused branch leaves the picker open on the rows it was refused
        // from, so the reason is read beside the other prompts rather than
        // after the list it applies to has closed.
        if let Err(error) = outcome {
            self.set_command_feedback(CommandFeedbackKind::Error, error, cx);
            return;
        }

        self.fork.state = Some(ForkState::Branching);
        self.history_ui.mode = RecentSessionsMode::Loading;
        self.history_ui.pending_resume_replay = None;
        self.status = Status::Starting;
        // The branch inherits the parent's controls, so nothing is seeded over
        // what its own history is about to replay.
        self.seed_thread_defaults = false;
        self.seed_approval_reviewer = false;

        // Cutting in front of a prompt leaves that prompt unasked, so it goes
        // back where it was typed rather than being dropped with the turns
        // after it.
        self.fork.pending_prompt = Some(checkpoint.prompt);

        self.set_command_feedback(
            CommandFeedbackKind::Notice,
            i18n("agent-session-forking").to_string(),
            cx,
        );
        cx.notify();
    }

    /// Close the branching state once the copy's own history has replaced the
    /// transcript, which is the moment the branch is something the user can
    /// type into.
    pub(in crate::agent) fn finish_conversation_branch(&mut self, cx: &mut Context<Self>) {
        if matches!(self.fork.state, Some(ForkState::Branching)) {
            self.fork.state = None;
            self.set_command_feedback(
                CommandFeedbackKind::Notice,
                i18n("agent-fork-complete").to_string(),
                cx,
            );
        }
    }

    /// Give up on a branch whose copy never arrived. The failure is reported by
    /// whatever the backend said went wrong, so this only releases the composer
    /// the branch was holding.
    pub(in crate::agent) fn abandon_conversation_branch(&mut self) {
        if matches!(self.fork.state, Some(ForkState::Branching)) {
            self.fork.state = None;
        }
    }
}

impl AgentPane {
    /// Put a cut prompt back in the composer, once there is a frame to do it
    /// in. Spent on the first render after the branch was requested.
    pub(in crate::agent) fn fill_branch_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.fork.pending_prompt.take() else {
            return;
        };

        self.input.update(cx, |input, cx| {
            input.set_value(prompt.clone(), window, cx);
            input.set_selected_range(prompt.len()..prompt.len(), cx);
        });
    }
}

fn cancel_row() -> PaletteRow {
    PaletteRow {
        label: i18n("agent-fork-cancel").to_string(),
        description: i18n("agent-fork-cancel-description").to_string(),
        hint: None,
        disabled_reason: None,
        action: PaletteAction::ForkCancel,
    }
}
