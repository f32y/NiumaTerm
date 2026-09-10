use crate::chat::ForkCheckpoint;
use crate::session::branch::{
    BranchError, BranchFailure, BranchUpdate, ConversationBranch, FailureStage, FileProgress,
    PromptTarget, State, checkpoint_at_depth,
};
use crate::session::{Backend, SessionRuntime};

impl ConversationBranch {
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
