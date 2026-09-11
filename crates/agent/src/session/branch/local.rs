use std::mem::replace;

use crate::chat::SlashCommandOutcome;
use crate::claude_code::sessions::{self, ClaudeCheckpoint, ClaudeFork, FileRestoreAvailability};
use crate::session::branch::{
    BranchError, BranchFailure, BranchUpdate, ConversationBranch, FailureStage, FileProgress,
    LocalOperation, LocalPhase, Operation, PromptTarget, RewindAction, Source, State,
    checkpoint_at_depth,
};
use crate::session::{AgentKind, Backend, RecoveryIdentity, SessionRuntime};

pub struct CheckpointRead {
    operation: Operation,
    source: Source,
}

impl CheckpointRead {
    /// Synchronous disk work for the caller's existing background executor.
    pub fn load(&self) -> Result<Vec<ClaudeCheckpoint>, String> {
        sessions::load_checkpoints(self.source.cwd.as_deref(), &self.source.id)
    }
}

pub struct ForkRequest {
    operation: Operation,
    source: Source,
    user_message_id: String,
}

impl ForkRequest {
    /// Uses the existing transcript algorithm and leaves the source file intact.
    pub fn run(&self) -> Result<ClaudeFork, String> {
        sessions::fork_session_before(
            self.source.cwd.as_deref(),
            &self.source.id,
            &self.user_message_id,
        )
    }
}

fn failure(stage: FailureStage, files: FileProgress, error: BranchError) -> BranchUpdate {
    BranchUpdate::Failed(BranchFailure {
        stage,
        files,
        error,
    })
}

fn fork_request(
    local: &mut LocalOperation,
    checkpoint: ClaudeCheckpoint,
    files: FileProgress,
) -> ForkRequest {
    let request = ForkRequest {
        operation: local.operation,
        source: local.source.clone(),
        user_message_id: checkpoint.user_message_id.clone(),
    };

    local.phase = LocalPhase::Forking { checkpoint, files };

    request
}

impl ConversationBranch {
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
}
