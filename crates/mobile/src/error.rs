use tokio::task::JoinError;
use tracing::warn;

/// A failure the app shows to the user. The text is already worded for
/// people, because the layers below phrase their errors that way.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("{message}")]
    Failed { message: String },
}

impl From<anyhow::Error> for CoreError {
    fn from(error: anyhow::Error) -> Self {
        warn!(error = %format_args!("{error:#}"), "mobile operation failed");

        // The alternate form keeps the context chain, the only part that
        // distinguishes "reaching the relay" from "the host refused this device".
        Self::Failed {
            message: format!("{error:#}"),
        }
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(error: serde_json::Error) -> Self {
        warn!(%error, "cannot decode host response");

        Self::Failed {
            message: format!("the host sent an unexpected reply: {error}"),
        }
    }
}

impl From<JoinError> for CoreError {
    fn from(error: JoinError) -> Self {
        warn!(%error, "mobile request stopped");

        Self::Failed {
            message: format!("the request stopped before it finished: {error}"),
        }
    }
}
