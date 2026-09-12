use nmt_agent::session::{OperationError, UnsupportedOperation};
use nmt_i18n::i18n;

pub(in crate::agent_tab) fn operation_error(error: OperationError) -> String {
    match error {
        OperationError::Failed(message) => message,

        OperationError::Unsupported(operation) => i18n(match operation {
            UnsupportedOperation::Rename => "agent-session-rename-unsupported",
            UnsupportedOperation::Fork => "agent-session-fork-unsupported",
            UnsupportedOperation::FileRewind => "agent-session-file-rewind-claude-only",
        })
        .to_string(),
    }
}
