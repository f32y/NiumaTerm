//! Branch and rewind operations independent of picker widgets and executors.

pub use crate::session::branch::local::{CheckpointRead, ForkRequest};

mod local;

#[cfg(test)]
mod tests;

use std::mem::replace;

use crate::chat::{ForkCheckpoint, ReplayTurn, SlashCommandOutcome};
use crate::claude_code::sessions::{ClaudeCheckpoint, ClaudeFork, FileRestoreAvailability};
use crate::session::branch::local::{failure, fork_request};
use crate::session::lifecycle::{SessionRuntime, Status};
use crate::session::{AgentKind, Backend, OperationError, RecoveryIdentity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptTarget {
    pub prompt: String,

    /// Distance from the newest turn-opening prompt.
    pub depth: usize,
}

/// A depth is accepted only when the text also agrees; compacted or omitted
/// turns otherwise risk cutting in front of a different prompt.
pub fn checkpoint_at_depth<'a, T>(
    checkpoints: &'a [T],
    target: &PromptTarget,
    prompt_of: impl Fn(&T) -> &str,
) -> Option<&'a T> {
    checkpoints
        .get(target.depth)
        .filter(|checkpoint| prompt_of(checkpoint) == target.prompt)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewindAction {
    Files,
    Conversation,
    FilesAndConversation,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileProgress {
    NotConfirmed,
    Restored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureStage {
    Checkpoints,
    Files,
    Conversation,
    Startup,
    ProtocolFork,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BranchError {
    Busy,
    NotReady,
    MissingSession,
    FilesUnavailable,
    InvalidFileResult(Option<String>),
    Operation(OperationError),
    Failed(String),
}

pub struct BranchFailure {
    pub stage: FailureStage,
    pub files: FileProgress,
    pub error: BranchError,
}

pub struct BranchCompletion {
    /// Protocol branches already carry their replay in the incoming event.
    pub replay: Option<Vec<ReplayTurn>>,

    pub prompt: String,
    pub files: FileProgress,
}

pub enum BranchUpdate {
    Ignored,
    Empty,
    Picker { unresolved: bool },
    Branching,
    RestoringFiles(RewindAction),
    CreateFork(ForkRequest),
    StartSession(Option<RecoveryIdentity>),
    FilesRestored,
    Failed(BranchFailure),
}

/// Borrowed picker content; callers cannot mutate the operation behind it.
pub enum BranchView<'a> {
    Idle,
    LoadingRewind,
    RewindCheckpoints(&'a [ClaudeCheckpoint]),
    RewindAction(&'a ClaudeCheckpoint, FileProgress),
    LoadingFork,
    ForkCheckpoints(&'a [ForkCheckpoint]),
    Working,
}

pub enum BranchReplay {
    Unrelated,
    Ignore,
    Complete(BranchCompletion),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Operation {
    id: u64,
    epoch: u64,
}

#[derive(Clone)]
struct Source {
    id: String,
    cwd: Option<String>,
}

struct LocalOperation {
    operation: Operation,
    source: Source,
    phase: LocalPhase,
}

enum LocalPhase {
    Loading(Option<PromptTarget>),
    Checkpoints(Vec<ClaudeCheckpoint>),
    Selecting {
        checkpoint: ClaudeCheckpoint,
        files: FileProgress,
    },
    Restoring {
        checkpoint: ClaudeCheckpoint,
        action: RewindAction,
    },
    Forking {
        checkpoint: ClaudeCheckpoint,
        files: FileProgress,
    },
    Prepared {
        identity: Option<RecoveryIdentity>,
        fork: ClaudeFork,
        prompt: String,
        files: FileProgress,
    },
    Starting {
        fork: ClaudeFork,
        prompt: String,
        files: FileProgress,
    },
}

enum State {
    Local(LocalOperation),
    LoadingFork {
        operation: Operation,
        target: Option<PromptTarget>,
    },
    ForkPicker {
        operation: Operation,
        checkpoints: Vec<ForkCheckpoint>,
    },
    Branching {
        operation: Operation,
        previous: Status,
        prompt: String,
    },
}

#[derive(Default)]
pub struct ConversationBranch {
    sequence: u64,
    state: Option<State>,
    // Checkpoint replies have no request id. A cancelled request is drained
    // before issuing another in the same session, so its reply cannot be reused.
    protocol_read: Option<Operation>,
    file_request: Option<Operation>,
}

impl ConversationBranch {
    pub fn holds_composer(&self) -> bool {
        self.state.is_some()
    }

    pub fn picker_is_open(&self) -> bool {
        matches!(
            self.into(),
            BranchView::LoadingRewind
                | BranchView::RewindCheckpoints(_)
                | BranchView::RewindAction(_, _)
                | BranchView::LoadingFork
                | BranchView::ForkCheckpoints(_)
        )
    }

    pub fn is_working(&self) -> bool {
        matches!(self.into(), BranchView::Working)
    }

    pub fn cancel_picker(&mut self) -> bool {
        if !self.picker_is_open() {
            return false;
        }

        self.state = None;

        true
    }

    /// Retire visible state without admitting a still-outstanding protocol reply.
    pub fn clear(&mut self) {
        self.state = None;
    }

    fn next_operation(&mut self, runtime: &SessionRuntime) -> Result<Operation, BranchError> {
        if self.state.is_some() || runtime.status() != Status::Idle {
            return Err(BranchError::Busy);
        }

        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("branch operation id exhausted");

        Ok(Operation {
            id: self.sequence,
            epoch: runtime.epoch(),
        })
    }

    /// Only the prepared local branch may carry replay across its own restart.
    pub fn starting(&mut self, epoch: u64, identity: Option<&RecoveryIdentity>) -> bool {
        self.protocol_read = None;
        self.file_request = None;

        self.state = match self.state.take() {
            Some(State::Local(mut local)) => match local.phase {
                LocalPhase::Prepared {
                    identity: expected,
                    fork,
                    prompt,
                    files,
                } if identity == expected.as_ref() => {
                    local.operation.epoch = epoch;

                    local.phase = LocalPhase::Starting {
                        fork,
                        prompt,
                        files,
                    };

                    Some(State::Local(local))
                }
                _ => None,
            },
            _ => None,
        };

        self.state.is_some()
    }

    pub fn ready(&mut self, epoch: u64) -> Option<BranchCompletion> {
        if !matches!(&self.state, Some(State::Local(local)) if local.operation.epoch == epoch &&
            matches!(local.phase, LocalPhase::Starting { .. }))
        {
            return None;
        }

        let Some(State::Local(local)) = self.state.take() else {
            unreachable!()
        };

        let LocalPhase::Starting {
            fork,
            prompt,
            files,
        } = local.phase
        else {
            unreachable!()
        };

        Some(BranchCompletion {
            replay: Some(fork.replay),
            prompt,
            files,
        })
    }

    pub fn replayed(&mut self, epoch: u64) -> BranchReplay {
        match &self.state {
            Some(State::Branching { operation, .. }) if operation.epoch == epoch => {
                let Some(State::Branching { prompt, .. }) = self.state.take() else {
                    unreachable!()
                };

                BranchReplay::Complete(BranchCompletion {
                    replay: None,
                    prompt,
                    files: FileProgress::NotConfirmed,
                })
            }
            Some(State::Local(_)) | Some(State::Branching { .. }) => BranchReplay::Ignore,
            _ => BranchReplay::Unrelated,
        }
    }

    pub fn failed(&mut self, runtime: &mut SessionRuntime, error: String) -> Option<BranchFailure> {
        let stage = match &self.state {
            Some(State::Branching { .. }) => FailureStage::ProtocolFork,
            Some(State::Local(local)) => match local.phase {
                LocalPhase::Restoring { .. } => FailureStage::Files,
                LocalPhase::Forking { .. } => FailureStage::Conversation,
                LocalPhase::Prepared { .. } | LocalPhase::Starting { .. } => FailureStage::Startup,
                _ => return None,
            },
            _ => return None,
        };

        let files = match self.state.take() {
            Some(State::Branching {
                operation,
                previous,
                ..
            }) => {
                if runtime.is_current(operation.epoch) {
                    runtime.conversation_change_rejected(previous);
                }

                FileProgress::NotConfirmed
            }
            Some(State::Local(local)) => match local.phase {
                LocalPhase::Forking { files, .. }
                | LocalPhase::Prepared { files, .. }
                | LocalPhase::Starting { files, .. } => files,
                _ => FileProgress::NotConfirmed,
            },
            _ => FileProgress::NotConfirmed,
        };

        Some(BranchFailure {
            stage,
            files,
            error: BranchError::Failed(error),
        })
    }

    pub fn begin_rewind(
        &mut self,
        runtime: &SessionRuntime,
        cwd: Option<String>,
        target: Option<PromptTarget>,
    ) -> Result<CheckpointRead, BranchError> {
        let operation = self.next_operation(runtime)?;

        let id = runtime
            .backend()
            .and_then(Backend::session_id)
            .ok_or(BranchError::MissingSession)?
            .to_owned();

        let source = Source { id, cwd };

        let request = CheckpointRead {
            operation,
            source: source.clone(),
        };

        self.state = Some(State::Local(LocalOperation {
            operation,
            source,
            phase: LocalPhase::Loading(target),
        }));

        Ok(request)
    }

    pub fn checkpoints_loaded(
        &mut self,
        epoch: u64,
        request: CheckpointRead,
        result: Result<Vec<ClaudeCheckpoint>, String>,
    ) -> BranchUpdate {
        let Some(State::Local(local)) = &mut self.state else {
            return BranchUpdate::Ignored;
        };

        if local.operation != request.operation || epoch != request.operation.epoch {
            return BranchUpdate::Ignored;
        }

        let LocalPhase::Loading(target) = &local.phase else {
            return BranchUpdate::Ignored;
        };

        match result {
            Ok(checkpoints) if checkpoints.is_empty() => {
                self.state = None;

                BranchUpdate::Empty
            }
            Ok(checkpoints) => {
                let selected = target
                    .as_ref()
                    .and_then(|target| checkpoint_at_depth(&checkpoints, target, |row| &row.prompt))
                    .cloned();

                let unresolved = target.is_some() && selected.is_none();

                local.phase = match selected {
                    Some(checkpoint) => LocalPhase::Selecting {
                        checkpoint,
                        files: FileProgress::NotConfirmed,
                    },
                    None => LocalPhase::Checkpoints(checkpoints),
                };

                BranchUpdate::Picker { unresolved }
            }
            Err(error) => {
                self.state = None;

                failure(
                    FailureStage::Checkpoints,
                    FileProgress::NotConfirmed,
                    BranchError::Failed(error),
                )
            }
        }
    }

    pub fn select_checkpoint(&mut self, epoch: u64, checkpoint: ClaudeCheckpoint) -> bool {
        let Some(State::Local(local)) = &mut self.state else {
            return false;
        };

        let LocalPhase::Checkpoints(checkpoints) = &local.phase else {
            return false;
        };

        if local.operation.epoch != epoch || !checkpoints.contains(&checkpoint) {
            return false;
        }

        local.phase = LocalPhase::Selecting {
            checkpoint,
            files: FileProgress::NotConfirmed,
        };

        true
    }

    pub fn rewind(&mut self, runtime: &mut SessionRuntime, action: RewindAction) -> BranchUpdate {
        if !matches!(&self.state, Some(State::Local(_))) {
            return BranchUpdate::Ignored;
        }

        let Some(State::Local(mut local)) = self.state.take() else {
            return BranchUpdate::Ignored;
        };

        if local.operation.epoch != runtime.epoch() {
            return BranchUpdate::Ignored;
        }

        if !matches!(local.phase, LocalPhase::Selecting { .. }) {
            self.state = Some(State::Local(local));

            return BranchUpdate::Ignored;
        }

        let LocalPhase::Selecting { checkpoint, files } =
            replace(&mut local.phase, LocalPhase::Checkpoints(Vec::new()))
        else {
            unreachable!()
        };

        match (action, files) {
            (RewindAction::Cancel, _) => return BranchUpdate::Ignored,
            (RewindAction::Conversation, _)
            | (RewindAction::FilesAndConversation, FileProgress::Restored) => {
                let request = fork_request(&mut local, checkpoint, files);

                self.state = Some(State::Local(local));

                return BranchUpdate::CreateFork(request);
            }
            (RewindAction::Files, FileProgress::Restored) => return BranchUpdate::FilesRestored,
            (
                RewindAction::Files | RewindAction::FilesAndConversation,
                FileProgress::NotConfirmed,
            ) => {}
        }

        let outcome = if self
            .file_request
            .is_some_and(|request| request.epoch == runtime.epoch())
        {
            Err(BranchError::Busy)
        } else if checkpoint.file_restore_availability == FileRestoreAvailability::Unavailable {
            Err(BranchError::FilesUnavailable)
        } else {
            runtime
                .backend_mut()
                .ok_or(BranchError::NotReady)
                .and_then(|backend| {
                    backend
                        .rewind_files(&checkpoint.user_message_id)
                        .map_err(BranchError::Operation)
                })
                .and_then(|outcome| match outcome {
                    SlashCommandOutcome::Accepted => Ok(()),
                    SlashCommandOutcome::NotReady => Err(BranchError::NotReady),
                    SlashCommandOutcome::Rejected { message } => Err(BranchError::Failed(message)),
                    SlashCommandOutcome::Completed { message } => {
                        Err(BranchError::InvalidFileResult(message))
                    }
                })
        };

        match outcome {
            Ok(()) => {
                self.file_request = Some(local.operation);
                local.phase = LocalPhase::Restoring { checkpoint, action };
                self.state = Some(State::Local(local));

                BranchUpdate::RestoringFiles(action)
            }
            Err(error) => {
                local.phase = LocalPhase::Selecting { checkpoint, files };
                self.state = Some(State::Local(local));

                failure(FailureStage::Files, files, error)
            }
        }
    }

    pub fn files_completed(&mut self, epoch: u64, result: Result<(), String>) -> BranchUpdate {
        let Some(request) = self.file_request else {
            return BranchUpdate::Ignored;
        };

        if request.epoch != epoch {
            return BranchUpdate::Ignored;
        }

        self.file_request = None;

        if !matches!(&self.state, Some(State::Local(local)) if local.operation == request &&
            matches!(local.phase, LocalPhase::Restoring { .. }))
        {
            return BranchUpdate::Ignored;
        }

        let Some(State::Local(mut local)) = self.state.take() else {
            unreachable!()
        };

        let LocalPhase::Restoring { checkpoint, action } =
            replace(&mut local.phase, LocalPhase::Checkpoints(Vec::new()))
        else {
            unreachable!()
        };

        match result {
            Ok(()) => match action {
                RewindAction::FilesAndConversation => {
                    let request = fork_request(&mut local, checkpoint, FileProgress::Restored);

                    self.state = Some(State::Local(local));

                    BranchUpdate::CreateFork(request)
                }
                _ => BranchUpdate::FilesRestored,
            },
            Err(error) => {
                local.phase = LocalPhase::Selecting {
                    checkpoint,
                    files: FileProgress::NotConfirmed,
                };

                self.state = Some(State::Local(local));

                failure(
                    FailureStage::Files,
                    FileProgress::NotConfirmed,
                    BranchError::Failed(error),
                )
            }
        }
    }

    pub fn fork_created(
        &mut self,
        epoch: u64,
        request: ForkRequest,
        result: Result<ClaudeFork, String>,
    ) -> BranchUpdate {
        if !matches!(&self.state, Some(State::Local(local)) if local.operation == request.operation &&
            local.operation.epoch == epoch && matches!(local.phase, LocalPhase::Forking { .. }))
        {
            return BranchUpdate::Ignored;
        }

        let Some(State::Local(mut local)) = self.state.take() else {
            unreachable!()
        };

        let LocalPhase::Forking { checkpoint, files } =
            replace(&mut local.phase, LocalPhase::Checkpoints(Vec::new()))
        else {
            unreachable!()
        };

        match result {
            Ok(fork) => {
                let identity = fork
                    .session_id
                    .as_ref()
                    .map(|id| RecoveryIdentity::new(AgentKind::Claude, id.clone()));

                local.phase = LocalPhase::Prepared {
                    identity: identity.clone(),
                    fork,
                    prompt: checkpoint.prompt,
                    files,
                };

                self.state = Some(State::Local(local));

                BranchUpdate::StartSession(identity)
            }
            Err(error) => {
                // Retain confirmed file restoration when retrying the remaining
                // conversation step; repeating the file operation could overwrite new edits.
                local.phase = LocalPhase::Selecting { checkpoint, files };
                self.state = Some(State::Local(local));

                failure(
                    FailureStage::Conversation,
                    files,
                    BranchError::Failed(error),
                )
            }
        }
    }

    pub fn begin_fork(
        &mut self,
        runtime: &mut SessionRuntime,
        target: Option<PromptTarget>,
    ) -> Result<(), BranchError> {
        if self
            .protocol_read
            .is_some_and(|request| request.epoch == runtime.epoch())
        {
            return Err(BranchError::Busy);
        }

        let operation = self.next_operation(runtime)?;

        if !runtime
            .backend_mut()
            .is_some_and(Backend::request_fork_checkpoints)
        {
            return Err(BranchError::NotReady);
        }

        self.protocol_read = Some(operation);
        self.state = Some(State::LoadingFork { operation, target });

        Ok(())
    }

    pub fn fork_checkpoints(
        &mut self,
        runtime: &mut SessionRuntime,
        result: Result<Vec<ForkCheckpoint>, String>,
    ) -> BranchUpdate {
        let Some(request) = self.protocol_read else {
            return BranchUpdate::Ignored;
        };

        if request.epoch != runtime.epoch() {
            return BranchUpdate::Ignored;
        }

        self.protocol_read = None;

        if !matches!(&self.state, Some(State::LoadingFork { operation, .. }) if *operation == request)
        {
            return BranchUpdate::Ignored;
        }

        let Some(State::LoadingFork { operation, target }) = self.state.take() else {
            unreachable!()
        };

        match result {
            Ok(checkpoints) if checkpoints.is_empty() => BranchUpdate::Empty,
            Ok(checkpoints) => {
                let selected = target
                    .as_ref()
                    .and_then(|target| checkpoint_at_depth(&checkpoints, target, |row| &row.prompt))
                    .cloned();

                let unresolved = target.is_some() && selected.is_none();

                self.state = Some(State::ForkPicker {
                    operation,
                    checkpoints,
                });

                match selected {
                    Some(checkpoint) => self.fork(runtime, checkpoint),
                    None => BranchUpdate::Picker { unresolved },
                }
            }
            Err(error) => BranchUpdate::Failed(BranchFailure {
                stage: FailureStage::Checkpoints,
                files: FileProgress::NotConfirmed,
                error: BranchError::Failed(error),
            }),
        }
    }

    pub fn fork(
        &mut self,
        runtime: &mut SessionRuntime,
        checkpoint: ForkCheckpoint,
    ) -> BranchUpdate {
        let Some(State::ForkPicker {
            operation,
            checkpoints,
        }) = &self.state
        else {
            return BranchUpdate::Ignored;
        };

        if operation.epoch != runtime.epoch() || !checkpoints.contains(&checkpoint) {
            return BranchUpdate::Ignored;
        }

        let result = runtime
            .backend_mut()
            .ok_or(BranchError::NotReady)
            .and_then(|backend| {
                backend
                    .fork_conversation(&checkpoint.anchor)
                    .map_err(BranchError::Operation)
            });

        match result {
            Ok(()) => {
                let previous = runtime.begin_conversation_change();

                self.state = Some(State::Branching {
                    operation: *operation,
                    previous,
                    prompt: checkpoint.prompt,
                });

                BranchUpdate::Branching
            }
            Err(error) => BranchUpdate::Failed(BranchFailure {
                stage: FailureStage::Conversation,
                files: FileProgress::NotConfirmed,
                error,
            }),
        }
    }
}

impl<'a> From<&'a ConversationBranch> for BranchView<'a> {
    fn from(value: &'a ConversationBranch) -> Self {
        match &value.state {
            None => BranchView::Idle,
            Some(State::LoadingFork { .. }) => BranchView::LoadingFork,
            Some(State::ForkPicker { checkpoints, .. }) => BranchView::ForkCheckpoints(checkpoints),
            Some(State::Branching { .. }) => BranchView::Working,
            Some(State::Local(local)) => match &local.phase {
                LocalPhase::Loading(_) => BranchView::LoadingRewind,
                LocalPhase::Checkpoints(checkpoints) => BranchView::RewindCheckpoints(checkpoints),
                LocalPhase::Selecting { checkpoint, files } => {
                    BranchView::RewindAction(checkpoint, *files)
                }
                _ => BranchView::Working,
            },
        }
    }
}
