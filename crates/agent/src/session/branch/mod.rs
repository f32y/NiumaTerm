//! Branch and rewind operations independent of picker widgets and executors.

use crate::chat::{ForkCheckpoint, ReplayTurn};
use crate::claude_code::sessions::{ClaudeCheckpoint, ClaudeFork};
use crate::session::lifecycle::{SessionRuntime, Status};
use crate::session::{OperationError, RecoveryIdentity};

mod local;
mod protocol;
#[cfg(test)]
mod tests;

pub use crate::session::branch::local::{CheckpointRead, ForkRequest};

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
    pub fn view(&self) -> BranchView<'_> {
        match &self.state {
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

    pub fn holds_composer(&self) -> bool {
        self.state.is_some()
    }

    pub fn picker_is_open(&self) -> bool {
        matches!(
            self.view(),
            BranchView::LoadingRewind
                | BranchView::RewindCheckpoints(_)
                | BranchView::RewindAction(_, _)
                | BranchView::LoadingFork
                | BranchView::ForkCheckpoints(_)
        )
    }

    pub fn is_working(&self) -> bool {
        matches!(self.view(), BranchView::Working)
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
}
