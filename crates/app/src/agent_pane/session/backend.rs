use nmt_agent_utils::claude_code::sessions::RestoredTask;
use nmt_i18n::i18n;

use crate::agent_pane::*;

/// The conversation a restarted backend should continue, qualified by the
/// harness that issued the id. Ids are only meaningful to the harness that
/// minted them, so a mismatched pair starts a fresh conversation instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryIdentity {
    pub(crate) kind: AgentKind,
    pub(crate) id: String,
}

impl RecoveryIdentity {
    pub(crate) fn new(kind: AgentKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

/// The pane's protocol session, one variant per agent kind. Both backends
/// share the [`nmt_agent_utils::chat`] event vocabulary and method surface,
/// so the pane dispatches here and stays protocol-agnostic.
pub(in crate::agent_pane) enum Backend {
    Codex(app_server::Session),
    Claude(stream_json::Session),
    #[cfg(test)]
    Test(TestBackend),
}

#[cfg(test)]
pub(in crate::agent_pane) struct TestBackend {
    send_outcomes: VecDeque<SendOutcome>,
    slash_outcome: SlashCommandOutcome,
    commands: Vec<SlashCommandInfo>,
}

#[cfg(test)]
impl TestBackend {
    pub(in crate::agent_pane) fn new(
        send_outcomes: impl IntoIterator<Item = SendOutcome>,
        slash_outcome: SlashCommandOutcome,
        commands: Vec<SlashCommandInfo>,
    ) -> Self {
        Self {
            send_outcomes: send_outcomes.into_iter().collect(),
            slash_outcome,
            commands,
        }
    }
}

impl Backend {
    /// Start the harness process for `kind` and wrap it in the matching
    /// variant. Resume differs by harness — Codex asks the running app-server
    /// to reopen a thread, Claude Code takes a session id as a launch flag —
    /// so the caller passes an identity and this decides how to use it.
    pub(in crate::agent_pane) fn spawn(
        kind: AgentKind,
        launch: &LaunchConfig,
        cwd: Option<String>,
        recovery: Option<RecoveryIdentity>,
        deliver: impl Fn(Value) + Send + 'static,
    ) -> Result<Self, String> {
        let resume = recovery
            .filter(|identity| identity.kind == kind)
            .map(|identity| identity.id);
        match kind {
            AgentKind::Codex => match resume {
                Some(thread_id) => app_server::Session::spawn_resuming(
                    launch,
                    cwd,
                    thread_id,
                    true,
                    deliver,
                    |line| warn!("codex app-server: {line}"),
                ),
                None => app_server::Session::spawn(launch, cwd, deliver, |line| {
                    warn!("codex app-server: {line}")
                }),
            }
            .map(Backend::Codex),
            AgentKind::Claude => {
                stream_json::Session::spawn(launch, cwd, resume, deliver, |line| {
                    warn!("claude: {line}")
                })
                .map(Backend::Claude)
            }
        }
    }

    pub(in crate::agent_pane) fn process(&mut self, message: Value) -> Vec<SessionEvent> {
        match self {
            Backend::Codex(session) => session.process(message),
            Backend::Claude(session) => session.process(message),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
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
            #[cfg(test)]
            Backend::Test(session) => session
                .send_outcomes
                .pop_front()
                .unwrap_or(SendOutcome::NotReady),
        }
    }

    pub(in crate::agent_pane) fn adapter_commands(&self) -> Vec<SlashCommandInfo> {
        match self {
            Backend::Codex(_) => app_server::Session::adapter_commands(),
            Backend::Claude(_) => stream_json::Session::adapter_commands(),
            #[cfg(test)]
            Backend::Test(session) => session.commands.clone(),
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
            #[cfg(test)]
            Backend::Test(session) => session.slash_outcome.clone(),
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
            #[cfg(test)]
            Backend::Test(session) => session.slash_outcome.clone(),
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
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    /// Ask the provider for one child's conversation. Codex reads the stored
    /// descendant thread; Claude Code reads the file the CLI wrote for that
    /// child, which is where a child's own turns live.
    pub(in crate::agent_pane) fn load_background_task_transcript(
        &mut self,
        key: &BackgroundTaskKey,
        cwd: Option<&str>,
    ) -> Vec<SessionEvent> {
        // A key names the provider that published the task, so a key belonging
        // to another harness reaches no session: its ids mean nothing here.
        match self {
            Backend::Codex(session) => match key.provider {
                BackgroundTaskProvider::Codex => session.load_background_task_transcript(&key.id),
                BackgroundTaskProvider::ClaudeCode => Vec::new(),
            },
            Backend::Claude(session) => match key.provider {
                BackgroundTaskProvider::ClaudeCode => {
                    session.load_background_task_transcript(&key.id, cwd)
                }
                BackgroundTaskProvider::Codex => Vec::new(),
            },
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    /// Take the sequence number a child-agent history read must not overwrite
    /// past. Live updates that land while the read runs keep their newer state.
    /// Only Claude Code rebuilds children from files, so Codex has no read to
    /// bracket and its sequence is unused.
    pub(in crate::agent_pane) fn begin_task_restoration(&mut self) -> u64 {
        match self {
            Backend::Claude(session) => session.begin_task_restoration(),
            Backend::Codex(_) => 0,
            #[cfg(test)]
            Backend::Test(_) => 0,
        }
    }

    pub(in crate::agent_pane) fn finish_task_restoration(
        &mut self,
        restored: Result<Vec<RestoredTask>, String>,
        starting_sequence: u64,
    ) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => {
                session.finish_task_restoration(restored, starting_sequence)
            }
            Backend::Codex(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    /// Continue an earlier conversation inside the running session. Returns
    /// whether the request reached a backend that can do it: Claude Code has no
    /// in-session resume and must respawn with the session id instead, so the
    /// caller keeps the recent-sessions list open and reports why.
    pub(in crate::agent_pane) fn resume_thread(&mut self, thread_id: &str) -> bool {
        match self {
            Backend::Codex(session) => {
                session.resume_thread(thread_id);
                true
            }
            Backend::Claude(_) => false,
            #[cfg(test)]
            Backend::Test(_) => false,
        }
    }

    /// Fetch the next page of recent sessions. Only Codex pages its history
    /// from the backend; Claude Code reads whole directories from disk.
    pub(in crate::agent_pane) fn request_more_history(&mut self) {
        match self {
            Backend::Codex(session) => session.request_more_history(),
            Backend::Claude(_) => {}
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    pub(in crate::agent_pane) fn session_id(&self) -> Option<&str> {
        match self {
            Backend::Claude(session) => session.session_id(),
            Backend::Codex(_) => None,
            #[cfg(test)]
            Backend::Test(_) => None,
        }
    }

    pub(in crate::agent_pane) fn recovery_identity(&self) -> Option<RecoveryIdentity> {
        match self {
            Backend::Claude(session) => session
                .session_id()
                .map(|id| RecoveryIdentity::new(AgentKind::Claude, id)),
            Backend::Codex(session) => session
                .thread_id()
                .map(|id| RecoveryIdentity::new(AgentKind::Codex, id)),
            #[cfg(test)]
            Backend::Test(_) => None,
        }
    }

    pub(in crate::agent_pane) fn has_active_operation(&self) -> bool {
        match self {
            Backend::Claude(session) => session.has_active_operation(),
            Backend::Codex(session) => session.has_active_operation(),
            #[cfg(test)]
            Backend::Test(_) => false,
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
            #[cfg(test)]
            Backend::Test(_) => Ok(()),
        }
    }

    pub(in crate::agent_pane) fn process_exit(&mut self) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => session.process_exit(),
            Backend::Codex(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    pub(in crate::agent_pane) fn interrupt(&mut self) {
        match self {
            Backend::Codex(session) => session.interrupt(),
            Backend::Claude(session) => session.interrupt(),
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    pub(in crate::agent_pane) fn respond_approval(&mut self, decision: &str) {
        match self {
            Backend::Codex(session) => session.respond_approval(decision),
            Backend::Claude(session) => session.respond_approval(decision),
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    /// What each still-running workflow run needs read on the next refresh
    /// tick. Only Claude reports workflows, so Codex has nothing to read.
    pub(in crate::agent_pane) fn workflow_refresh_requests(&self) -> Vec<WorkflowRefreshRequest> {
        match self {
            Backend::Claude(session) => session.workflow_refresh_requests(),
            Backend::Codex(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    pub(in crate::agent_pane) fn apply_workflow_refresh(
        &mut self,
        result: WorkflowRefreshResult,
    ) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => session.apply_workflow_refresh(result),
            Backend::Codex(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    pub(in crate::agent_pane) fn restore_workflows(
        &mut self,
        restored: Vec<RestoredWorkflowRun>,
    ) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => session.restore_workflows(restored),
            Backend::Codex(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    /// Answer an `AskUserQuestion` card. Only Claude asks structured questions;
    /// Codex has no equivalent request, so there is nothing to answer there.
    pub(in crate::agent_pane) fn respond_questions(&mut self, answers: Option<Vec<Vec<String>>>) {
        match self {
            Backend::Claude(session) => session.respond_questions(answers),
            Backend::Codex(_) => {}
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }
}
