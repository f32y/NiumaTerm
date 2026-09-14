use crate::claude_code::sessions;
use crate::claude_code::sessions::{ClaudeCheckpoint, ClaudeFork};
use crate::session::branch::{
    BranchError, BranchFailure, BranchUpdate, FailureStage, FileProgress, LocalOperation,
    LocalPhase, Operation, Source,
};

pub struct CheckpointRead {
    pub(super) operation: Operation,
    pub(super) source: Source,
}

impl CheckpointRead {
    /// Synchronous disk work for the caller's existing background executor.
    pub fn load(&self) -> Result<Vec<ClaudeCheckpoint>, String> {
        sessions::load_checkpoints(self.source.cwd.as_deref(), &self.source.id)
    }
}

pub struct ForkRequest {
    pub(super) operation: Operation,
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

pub(super) fn failure(
    stage: FailureStage,
    files: FileProgress,
    error: BranchError,
) -> BranchUpdate {
    BranchUpdate::Failed(BranchFailure {
        stage,
        files,
        error,
    })
}

pub(super) fn fork_request(
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
