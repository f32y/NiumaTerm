//! Branching a conversation in front of a chosen prompt, for the backends
//! whose history lives behind their connection.
//!
//! Claude's history is a file this side reads and rewrites itself, and its
//! rewind picker offers the same cut alongside restoring the files that turn
//! touched; `/fork` opens that picker there rather than a second one that
//! would do less. What is left here is the shape the other two share: ask the
//! backend which prompts it can branch in front of, show them, and hand the
//! chosen one back so the branch starts where it was cut.

use gpui::{Context, SharedString, Window};
use nmt_agent_utils::chat::ForkCheckpoint;
use nmt_i18n::i18n;

use crate::composer::branch::rewind::{rewind_prompt_label, rewind_timestamp};
use crate::composer::{CommandFeedbackKind, PaletteAction, PaletteModel, PaletteRow, RewindState};
use crate::session::{Backend, Status};
use crate::settings::AgentSettings;
use crate::{AgentPane, RecentSessionsMode, translated};

/// One prompt the user pointed at in the transcript, to be found in the list
/// of branch points once the backend answers with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptTarget {
    pub(crate) prompt: String,
    /// How many turn-opening prompts follow it, which is what locates it from
    /// the newest end of that list.
    pub(crate) depth: usize,
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
pub(crate) fn checkpoint_at_depth<'a, T>(
    checkpoints: &'a [T],
    target: &PromptTarget,
    prompt_of: impl Fn(&T) -> &str,
) -> Option<&'a T> {
    checkpoints
        .get(target.depth)
        .filter(|checkpoint| prompt_of(checkpoint) == target.prompt)
}

/// Name the prompt one picker row stands for.
///
/// Both pickers list their branch points newest first and append their cancel
/// row last, which is the same order `depth` counts in, so a row's own index
/// is the depth of the prompt it offers. The text travels with it, so the
/// transcript can refuse to move if the two lists have drifted apart.
pub(crate) fn row_prompt_target(row: usize, action: &PaletteAction) -> Option<PromptTarget> {
    let prompt = match action {
        PaletteAction::RewindCheckpoint(checkpoint) => checkpoint.prompt.clone(),
        PaletteAction::ForkCheckpoint(checkpoint) => checkpoint.prompt.clone(),
        _ => return None,
    };

    Some(PromptTarget { prompt, depth: row })
}

/// A branch is a local multi-step operation rather than a model turn, so its
/// progress is tracked apart from the turn state; timers, transcript rows, and
/// the slash queue must not read cutting a branch as provider output.
///
/// One request is outstanding at a time — the composer is held while the
/// picker is up, so a second `/fork` cannot be sent — which is why the answer
/// needs no request identity to be matched against.
pub(crate) enum ForkState {
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
    pub(crate) fn is_picker(&self) -> bool {
        matches!(self, Self::Loading(_) | Self::Selecting(_))
    }
}

#[derive(Default)]
pub(crate) struct ForkFlow {
    pub(crate) state: Option<ForkState>,
    /// The prompt the branch was cut in front of, waiting for a frame to put
    /// it back in the composer. A branch can be settled from the backend's
    /// answer, which arrives with no window to reach the input through, so the
    /// text is parked here and the next render spends it.
    pub(crate) pending_prompt: Option<String>,
}

impl AgentPane {
    /// Whether such a flow is past its picker and working. Until then the
    /// input still holds text worth editing, so only sending is refused.
    pub(crate) fn branch_flow_is_working(&self) -> bool {
        self.branch.is_working()
    }

    /// Whether a list of branch points is on screen, which is what makes the
    /// palette's highlight something the transcript follows.
    pub(crate) fn branch_picker_is_open(&self) -> bool {
        self.branch.picker_is_open()
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
    /// Following the smooth-scrolling setting keeps the jump between two
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

        let smooth = cx.global::<AgentSettings>().smooth_wheel;
        self.transcript.update(cx, |transcript, cx| {
            transcript.scroll_to_prompt(&target, smooth, cx)
        });
    }

    /// Close whichever picker is open, answering whether there was one. Escape
    /// reaches both, and only one can be open at a time.
    pub(crate) fn cancel_branch_picker(&mut self, cx: &mut Context<Self>) -> bool {
        if self
            .branch
            .rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.cancel_rewind_picker(cx);
            return true;
        }
        if self
            .branch
            .fork
            .state
            .as_ref()
            .is_some_and(ForkState::is_picker)
        {
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
    pub(crate) fn fork_from_prompt(
        &mut self,
        target: PromptTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        self.request_fork_checkpoints(Some(target), cx)
    }

    /// Ask the backend which prompts this conversation can be branched in
    /// front of, and open the picker on the answer.
    pub(crate) fn open_fork(&mut self, cx: &mut Context<Self>) -> bool {
        self.request_fork_checkpoints(None, cx)
    }

    fn request_fork_checkpoints(
        &mut self,
        target: Option<PromptTarget>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.runtime.status != Status::Idle || self.is_command_busy() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                translated("agent-fork-idle-only"),
                cx,
            );
            return false;
        }

        let asked = self
            .runtime
            .backend
            .as_mut()
            .is_some_and(Backend::request_fork_checkpoints);
        if !asked {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-still-starting").replace("{name}", self.kind.display()),
                cx,
            );
            return false;
        }

        self.branch.fork.state = Some(ForkState::Loading(target));
        self.palette.selected = 0;
        self.palette.dismissed = false;
        self.palette.set_feedback(
            CommandFeedbackKind::Status,
            translated("agent-fork-loading-checkpoints"),
            cx,
        );
        true
    }

    /// Take the backend's answer to the open request.
    ///
    /// An answer to a request the user has already cancelled is dropped: it
    /// would otherwise reopen a picker they closed, and a second `/fork` has
    /// its own answer coming.
    pub(crate) fn show_fork_checkpoints(
        &mut self,
        checkpoints: Result<Vec<ForkCheckpoint>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(ForkState::Loading(target)) = self.branch.fork.state.take() else {
            return;
        };

        match checkpoints {
            Ok(checkpoints) if checkpoints.is_empty() => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    translated("agent-fork-no-prompts"),
                    cx,
                );
            }
            Ok(checkpoints) => {
                let pointed_at = target.as_ref().and_then(|target| {
                    checkpoint_at_depth(&checkpoints, target, |checkpoint| &checkpoint.prompt)
                        .cloned()
                });
                self.branch.fork.state = Some(ForkState::Selecting(checkpoints));
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
                            self.palette.set_feedback(
                                CommandFeedbackKind::Error,
                                translated("agent-fork-prompt-not-a-branch-point"),
                                cx,
                            );
                        }
                        self.hold_transcript_for_picker(cx);
                        // The newest prompt is highlighted, and it usually
                        // sits under the picker that just opened over the
                        // bottom of the transcript.
                        self.follow_branch_selection(cx);
                        cx.notify();
                    }
                }
            }
            Err(message) => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
            }
        }
    }

    pub(crate) fn cancel_fork_picker(&mut self, cx: &mut Context<Self>) {
        if self
            .branch
            .fork
            .state
            .as_ref()
            .is_some_and(ForkState::is_picker)
        {
            self.branch.fork.state = None;
            self.palette.selected = 0;
            self.release_transcript_from_picker(cx);
            // Cancelling is the user's own no-op, and dropping the message
            // retires the non-transient "Reading branch points…" status that
            // would otherwise outlive the picker it described.
            self.palette.feedback = None;
            cx.notify();
        }
    }

    pub(crate) fn fork_palette_model(&self, state: &ForkState) -> Option<PaletteModel> {
        match state {
            ForkState::Loading(_) => Some(PaletteModel {
                rows: vec![cancel_row()],
                note: Some(translated("agent-fork-loading-checkpoints")),
            }),
            ForkState::Selecting(checkpoints) => {
                let mut rows = checkpoints
                    .iter()
                    .cloned()
                    .map(|checkpoint| PaletteRow {
                        label: rewind_prompt_label(&checkpoint.prompt).into(),
                        description: translated("agent-fork-branch-before-prompt"),
                        hint: rewind_timestamp(checkpoint.timestamp.as_deref())
                            .map(SharedString::from),
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
            ForkState::Branching => None,
        }
    }

    /// Branch in front of the chosen prompt and move the tab into the copy.
    ///
    /// The tab moves the same way it moves into a resumed conversation, so the
    /// presentation waits on the branch's own replay: what is visible stays
    /// the conversation it branched from until the copy's history arrives to
    /// replace it, and a refused branch leaves the tab exactly where it was.
    pub(crate) fn start_conversation_branch(
        &mut self,
        checkpoint: ForkCheckpoint,
        cx: &mut Context<Self>,
    ) {
        let outcome = match self.runtime.backend.as_mut() {
            Some(session) => session.fork_conversation(&checkpoint.anchor),
            None => {
                Err(i18n("agent-session-still-starting").replace("{name}", self.kind.display()))
            }
        };

        // A refused branch leaves the picker open on the rows it was refused
        // from, so the reason is read beside the other prompts rather than
        // after the list it applies to has closed.
        if let Err(error) = outcome {
            self.palette
                .set_feedback(CommandFeedbackKind::Error, error, cx);
            return;
        }

        self.branch.fork.state = Some(ForkState::Branching);
        self.history_ui.mode = RecentSessionsMode::Loading;
        self.history_ui.pending_resume_replay = None;
        self.runtime.status = Status::Starting;
        // The branch inherits the parent's controls, so nothing is seeded over
        // what its own history is about to replay.
        self.controls.seed_thread_defaults = false;
        self.controls.seed_approval_reviewer = false;

        // Cutting in front of a prompt leaves that prompt unasked, so it goes
        // back where it was typed rather than being dropped with the turns
        // after it.
        self.branch.fork.pending_prompt = Some(checkpoint.prompt);

        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            translated("agent-session-forking"),
            cx,
        );
        cx.notify();
    }

    /// Close the branching state once the copy's own history has replaced the
    /// transcript, which is the moment the branch is something the user can
    /// type into.
    pub(crate) fn finish_conversation_branch(&mut self, cx: &mut Context<Self>) {
        if matches!(self.branch.fork.state, Some(ForkState::Branching)) {
            self.branch.fork.state = None;
            self.palette.set_feedback(
                CommandFeedbackKind::Notice,
                translated("agent-fork-complete"),
                cx,
            );
        }
    }

    /// Give up on a branch whose copy never arrived. The failure is reported by
    /// whatever the backend said went wrong, so this only releases the composer
    /// the branch was holding.
    pub(crate) fn abandon_conversation_branch(&mut self) {
        if matches!(self.branch.fork.state, Some(ForkState::Branching)) {
            self.branch.fork.state = None;
        }
    }
}

impl AgentPane {
    /// Put a cut prompt back in the composer, once there is a frame to do it
    /// in. Spent on the first render after the branch was requested.
    pub(crate) fn fill_branch_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.branch.fork.pending_prompt.take() else {
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
        label: translated("agent-fork-cancel"),
        description: translated("agent-fork-cancel-description"),
        hint: None,
        disabled_reason: None,
        action: PaletteAction::ForkCancel,
    }
}
