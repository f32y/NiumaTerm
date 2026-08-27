//! Actions that address the conversation itself rather than its contents:
//! its title, its branches, the search over its siblings, and the prompts
//! waiting behind the running turn.

use nmt_i18n::i18n;

use crate::agent::composer::CommandFeedbackKind;
use crate::agent::*;

/// Which of the prompts this side is holding a new pending-inbox snapshot no
/// longer names.
///
/// A prompt the backend has stopped listing is one it has claimed into the
/// running turn, which is the moment its transcript row is due. Rows are
/// matched on their text rather than on the backend's identity because a
/// prompt this side queued optimistically has no identity yet, and treating it
/// as claimed on the very first snapshot would show it as sent while it was
/// still waiting.
pub(in crate::agent) fn claimed_prompts(
    held: &VecDeque<QueuedPrompt>,
    pending: &[QueuedPrompt],
) -> Vec<String> {
    held.iter()
        .filter(|held| !pending.iter().any(|pending| pending.text == held.text))
        .map(|held| held.text.clone())
        .collect()
}

impl AgentPane {
    /// Pin a title on this conversation.
    ///
    /// An empty title is refused here rather than sent, because a backend that
    /// normalizes it away answers the same refusal after a round trip and the
    /// composer would have discarded the line in the meantime.
    pub(in crate::agent) fn rename_conversation(
        &mut self,
        title: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = title.trim();
        if title.is_empty() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-rename-needs-title").to_string(),
                cx,
            );
            return false;
        }

        let outcome = match self.runtime.backend.as_mut() {
            Some(session) => session.rename_conversation(title),
            None => {
                Err(i18n("agent-session-still-starting").replace("{name}", self.kind.display()))
            }
        };

        // The accepted title is echoed rather than the requested one: the
        // backend normalizes what it stores, and confirming text it did not
        // keep would describe a rename that did not happen that way.
        match outcome {
            Ok(accepted) => {
                self.set_command_feedback(
                    CommandFeedbackKind::Notice,
                    i18n("agent-session-renamed").replace("{title}", &accepted),
                    cx,
                );
                true
            }
            Err(error) => {
                self.set_command_feedback(CommandFeedbackKind::Error, error, cx);
                false
            }
        }
    }

    /// Ask the backend which earlier conversations mention a phrase.
    ///
    /// The answer replaces the recent list, so the list is opened here and the
    /// arriving results land in a surface the user is already looking at
    /// rather than one they would have to go and find.
    pub(in crate::agent) fn search_conversations(
        &mut self,
        query: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let query = query.trim();
        if query.is_empty() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-search-needs-query").to_string(),
                cx,
            );
            return false;
        }

        let Some(session) = self.runtime.backend.as_mut() else {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-still-starting").replace("{name}", self.kind.display()),
                cx,
            );
            return false;
        };

        session.search_sessions(query);
        self.set_command_feedback(
            CommandFeedbackKind::Notice,
            i18n("agent-session-searching").replace("{query}", query),
            cx,
        );
        true
    }

    /// Show what one search matched, in place of whatever the list held.
    ///
    /// An empty result set keeps the list closed and says so, because opening
    /// an empty strip would read as a list that failed to load.
    pub(in crate::agent) fn show_search_results(
        &mut self,
        results: Vec<SessionSummary>,
        cx: &mut Context<Self>,
    ) {
        if results.is_empty() {
            self.set_command_feedback(
                CommandFeedbackKind::Notice,
                i18n("agent-session-search-no-matches").to_string(),
                cx,
            );
            return;
        }

        let count = results.len();
        self.history_ui.sessions = results;
        self.history_ui.showing_search = true;
        self.history_ui.pending = None;
        self.history_ui.selected = 0;
        self.history_ui.mode = RecentSessionsMode::Open;
        self.set_command_feedback(
            CommandFeedbackKind::Notice,
            i18n("agent-session-search-matches").replace("{count}", &count.to_string()),
            cx,
        );
    }

    /// Drop one prompt waiting behind the running turn.
    ///
    /// The row stays until the backend confirms the removal: a message it has
    /// already claimed is one the transcript is about to show as sent, and
    /// removing the row first would make it look like it never went.
    pub(in crate::agent) fn remove_queued_prompt(&mut self, item_id: &str, cx: &mut Context<Self>) {
        let removed = self
            .runtime
            .backend
            .as_mut()
            .is_some_and(|session| session.remove_queued_prompt(item_id));

        if !removed {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-session-queued-remove-failed").to_string(),
                cx,
            );
            return;
        }

        self.turn
            .queued_user_messages
            .retain(|queued| queued.id.as_deref() != Some(item_id));
        cx.notify();
    }
}
