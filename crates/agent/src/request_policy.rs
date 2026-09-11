use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestClass {
    Query,
    Mutation,
    Control,
}

impl RequestClass {
    // Deadlines start at admission. A timed out operation can still be waiting
    // in the writer, so expiration cannot establish that it was never applied.
    pub(crate) fn timeout(self) -> Duration {
        Duration::from_secs(match self {
            Self::Query => 30,
            Self::Mutation => 300,
            Self::Control => 15,
        })
    }

    pub(crate) fn timeout_message(self, provider: &str) -> String {
        match self {
            Self::Query => format!("{provider} query timed out. You can retry the query."),

            Self::Mutation | Self::Control => format!(
                "{provider} request timed out. The operation may still complete; its result is unknown. Check its state before retrying."
            ),
        }
    }
}
