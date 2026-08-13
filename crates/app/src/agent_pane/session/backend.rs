use nmt_i18n::i18n;

use crate::agent_pane::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryIdentity {
    NewConversation,
    ClaudeSession(String),
    CodexThread(String),
}

/// The pane's protocol session, one variant per agent kind. Both backends
/// share the [`nmt_agent_utils::chat`] event vocabulary and method surface,
/// so the pane dispatches here and stays protocol-agnostic.
pub(in crate::agent_pane) enum Backend {
    Codex(app_server::Session),
    Claude(stream_json::Session),
}

impl Backend {
    pub(in crate::agent_pane) fn process(&mut self, message: Value) -> Vec<SessionEvent> {
        match self {
            Backend::Codex(session) => session.process(message),
            Backend::Claude(session) => session.process(message),
        }
    }

    pub(in crate::agent_pane) fn send_user_message(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
    ) -> SendOutcome {
        match self {
            Backend::Codex(session) => session.send_user_message_with_skill(text, settings, skill),
            Backend::Claude(session) => session.send_user_message(text, settings),
        }
    }

    pub(in crate::agent_pane) fn adapter_commands(&self) -> Vec<SlashCommandInfo> {
        match self {
            Backend::Codex(_) => app_server::Session::adapter_commands(),
            Backend::Claude(_) => stream_json::Session::adapter_commands(),
        }
    }

    pub(in crate::agent_pane) fn execute_slash_command(
        &mut self,
        name: &str,
        arguments: &str,
    ) -> SlashCommandOutcome {
        match self {
            Backend::Codex(session) => session.execute_slash_command(name, arguments),
            Backend::Claude(session) => session.execute_slash_command(name, arguments),
        }
    }

    pub(in crate::agent_pane) fn rewind_files(
        &mut self,
        user_message_id: &str,
    ) -> SlashCommandOutcome {
        match self {
            Backend::Claude(session) => session.rewind_files(user_message_id),
            Backend::Codex(_) => SlashCommandOutcome::Rejected {
                message: i18n("agent-session-file-rewind-claude-only").to_string(),
            },
        }
    }

    /// Ask the provider for fresher child-agent data. Adapters guard against
    /// overlapping requests themselves, so opening the panel repeatedly cannot
    /// queue duplicate discovery passes.
    pub(in crate::agent_pane) fn refresh_background_tasks(&mut self) {
        match self {
            Backend::Codex(session) => session.refresh_background_tasks(),
            // Claude Code rebuilds tasks from session history rather than a
            // provider query, so there is nothing to re-request live.
            Backend::Claude(_) => {}
        }
    }

    /// Ask the provider for one child's conversation. Codex reads the stored
    /// descendant thread; Claude Code accumulates linked activity as it
    /// arrives, so there is nothing extra to request.
    pub(in crate::agent_pane) fn load_background_task_transcript(
        &mut self,
        key: &BackgroundTaskKey,
    ) -> Vec<SessionEvent> {
        match (self, key.provider) {
            (Backend::Codex(session), BackgroundTaskProvider::Codex) => {
                session.load_background_task_transcript(&key.id)
            }
            _ => Vec::new(),
        }
    }

    pub(in crate::agent_pane) fn session_id(&self) -> Option<&str> {
        match self {
            Backend::Claude(session) => session.session_id(),
            Backend::Codex(_) => None,
        }
    }

    pub(in crate::agent_pane) fn recovery_identity(&self) -> Option<RecoveryIdentity> {
        match self {
            Backend::Claude(session) => session
                .session_id()
                .map(|id| RecoveryIdentity::ClaudeSession(id.to_string())),
            Backend::Codex(session) => session
                .thread_id()
                .map(|id| RecoveryIdentity::CodexThread(id.to_string())),
        }
    }

    pub(in crate::agent_pane) fn has_active_operation(&self) -> bool {
        match self {
            Backend::Claude(session) => session.has_active_operation(),
            Backend::Codex(session) => session.has_active_operation(),
        }
    }

    pub(in crate::agent_pane) fn shutdown(
        &mut self,
        timeout: Duration,
        force: bool,
    ) -> Result<(), String> {
        match self {
            Backend::Claude(session) => session.shutdown(timeout, force),
            Backend::Codex(session) => session.shutdown(timeout, force),
        }
    }

    pub(in crate::agent_pane) fn process_exit(&mut self) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => session.process_exit(),
            Backend::Codex(_) => Vec::new(),
        }
    }

    pub(in crate::agent_pane) fn interrupt(&mut self) {
        match self {
            Backend::Codex(session) => session.interrupt(),
            Backend::Claude(session) => session.interrupt(),
        }
    }

    pub(in crate::agent_pane) fn respond_approval(&mut self, decision: &str) {
        match self {
            Backend::Codex(session) => session.respond_approval(decision),
            Backend::Claude(session) => session.respond_approval(decision),
        }
    }
}
