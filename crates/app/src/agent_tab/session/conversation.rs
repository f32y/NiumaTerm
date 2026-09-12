//! Actions that address the conversation itself rather than its contents:
//! its title, its branches, the search over its siblings, and the prompts
//! waiting behind the running turn.

use gpui::Context;
use nmt_agent::chat::SessionSummary;
use rust_i18n::t;

use crate::agent_tab::composer::CommandFeedbackKind;
use crate::agent_tab::session::errors::operation_error;
use crate::agent_tab::{AgentPane, RecentSessionsMode};

impl AgentPane {
    /// Pin a title on this conversation.
    ///
    /// An empty title is refused here rather than sent, because a backend that
    /// normalizes it away answers the same refusal after a round trip and the
    /// composer would have discarded the line in the meantime.
    pub(in crate::agent_tab) fn rename_conversation(
        &mut self,
        title: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        let title = title.trim();

        if title.is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-rename-needs-title").to_string(),
                cx,
            );

            return false;
        }

        let outcome = match self.session.borrow_mut().runtime.backend_mut() {
            Some(session) => session.rename_conversation(title).map_err(operation_error),

            None => {
                Err(t!("agent-session-still-starting", name = self.kind.display()).into_owned())
            }
        };

        // The accepted title is echoed rather than the requested one: the
        // backend normalizes what it stores, and confirming text it did not
        // keep would describe a rename that did not happen that way.
        match outcome {
            Ok(accepted) => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    t!("agent-session-renamed", title = &accepted).into_owned(),
                    cx,
                );

                true
            }

            Err(error) => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, error, cx);

                false
            }
        }
    }

    /// Ask the backend which earlier conversations mention a phrase.
    ///
    /// The answer replaces the recent list, so the list is opened here and the
    /// arriving results land in a surface the user is already looking at
    /// rather than one they would have to go and find.
    pub(in crate::agent_tab) fn search_conversations(
        &mut self,
        query: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        let query = query.trim();

        if query.is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-search-needs-query").to_string(),
                cx,
            );

            return false;
        }

        let mut state = self.session.borrow_mut();

        let Some(session) = state.runtime.backend_mut() else {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-still-starting", name = self.kind.display()).into_owned(),
                cx,
            );

            return false;
        };

        session.search_sessions(query);

        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            t!("agent-session-searching", query = query).into_owned(),
            cx,
        );

        true
    }

    /// Show what one search matched, in place of whatever the list held.
    ///
    /// An empty result set keeps the list closed and says so, because opening
    /// an empty strip would read as a list that failed to load.
    pub(in crate::agent_tab) fn show_search_results(
        &mut self,
        results: Vec<SessionSummary>,
        cx: &mut Context<Self>,
    ) {
        let count = results.len();

        if !self.history_ui.data.search_results(results) {
            self.palette.set_feedback(
                CommandFeedbackKind::Notice,
                t!("agent-session-search-no-matches").to_string(),
                cx,
            );

            return;
        }

        self.history_ui.selected = 0;
        self.history_ui.mode = RecentSessionsMode::Open;

        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            t!("agent-session-search-matches", count = count).into_owned(),
            cx,
        );
    }

    /// Drop one prompt waiting behind the running turn.
    ///
    /// The row stays until the backend confirms the removal: a message it has
    /// already claimed is one the transcript is about to show as sent, and
    /// removing the row first would make it look like it never went.
    pub(in crate::agent_tab) fn remove_queued_prompt(
        &mut self,
        item_id: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        let removed = self
            .session
            .borrow_mut()
            .runtime
            .backend_mut()
            .is_some_and(|session| session.remove_queued_prompt(item_id));

        if !removed {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-session-queued-remove-failed").to_string(),
                cx,
            );

            return;
        }

        self.session.borrow_mut().delivery.removed(item_id);

        cx.notify();
    }
}
