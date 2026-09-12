use nmt_agent::session::{OperationError, UnsupportedOperation};
use rust_i18n::t;

pub(crate) fn operation_error(error: OperationError) -> String {
    match error {
        OperationError::Failed(message) => message,

        OperationError::Unsupported(operation) => t!(match operation {
            UnsupportedOperation::Rename => "agent-session-rename-unsupported",
            UnsupportedOperation::Fork => "agent-session-fork-unsupported",
            UnsupportedOperation::FileRewind => "agent-session-file-rewind-claude-only",
        })
        .to_string(),
    }
}
