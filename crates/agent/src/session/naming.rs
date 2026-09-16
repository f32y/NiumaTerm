use std::iter;

use crate::claude_code::sessions as claude_sessions;
use crate::codex::app_server;
use crate::session::{AgentKind, Backend, ConversationTitleRequest, RenameOutcome};

/// Longest provisional title, in characters including the ellipsis, so the
/// tab strip and history rows stay one line.
const PROVISIONAL_TITLE_CHARS: usize = 60;

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

/// The compact title a conversation shows until its provider generates one,
/// derived from the opening prompt. Whitespace runs collapse to single
/// spaces, at most `max_words` words are kept, and longer text is cut with an
/// ellipsis. A prompt that opens with `/` names a command rather than a
/// subject, so it yields no title.
pub(crate) fn provisional_title(text: &str, max_words: Option<usize>) -> Option<String> {
    let mut words = text.split_whitespace();

    let first = words.next()?;

    if first.starts_with('/') {
        return None;
    }

    let normalized = iter::once(first)
        .chain(words)
        .take(max_words.unwrap_or(usize::MAX))
        .collect::<Vec<_>>()
        .join(" ");

    let mut chars = normalized.chars();

    let prefix: String = chars.by_ref().take(PROVISIONAL_TITLE_CHARS).collect();

    if chars.next().is_none() {
        return Some(prefix);
    }

    let mut truncated: String = prefix.chars().take(PROVISIONAL_TITLE_CHARS - 1).collect();

    // A cut that lands after a space would render as "word …"; trimming
    // keeps the ellipsis attached to the last kept word.
    truncated.truncate(truncated.trim_end().len());

    truncated.push('…');

    Some(truncated)
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
