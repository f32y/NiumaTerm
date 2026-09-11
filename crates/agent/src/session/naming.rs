use crate::claude_code::sessions as claude_sessions;
use crate::codex::app_server;
use crate::session::{AgentKind, Backend, ConversationTitleRequest, RenameOutcome};

#[derive(Default)]
pub struct ConversationNaming {
    pub named: bool,

    /// A rename remains pending until the provider can address and accept it.
    pub pending: Option<String>,
}

pub fn conversation_title_request(
    kind: AgentKind,
    text: &str,
    fallback: impl FnOnce(&str) -> Option<String>,
) -> Option<ConversationTitleRequest> {
    let provisional_title = match kind {
        AgentKind::Codex => app_server::provisional_title_from_prompt(text),
        AgentKind::Claude => claude_sessions::provisional_title_from_prompt(text),
        AgentKind::DeepSeek => fallback(text),
    }?;

    Some(ConversationTitleRequest {
        description: text.to_string(),
        provisional_title,
    })
}

impl ConversationNaming {
    pub fn request(
        &self,
        kind: AgentKind,
        text: &str,
        fallback: impl FnOnce(&str) -> Option<String>,
    ) -> Option<ConversationTitleRequest> {
        if self.named {
            None
        } else {
            conversation_title_request(kind, text, fallback)
        }
    }

    pub fn rename(&mut self, title: &str) {
        self.named = true;
        self.pending = Some(title.to_owned());
    }

    pub fn sync(&mut self, backend: Option<&mut Backend>) {
        let can_address_conversation = backend
            .as_deref()
            .and_then(Backend::recovery_identity)
            .is_some();

        if can_address_conversation
            && let Some(session) = backend
            && let Some(title) = self.pending.as_deref()
        {
            match session.rename_session(title) {
                RenameOutcome::Accepted | RenameOutcome::Unsupported => {
                    self.pending = None;
                }

                RenameOutcome::Rejected => {}
            }
        }
    }
}
