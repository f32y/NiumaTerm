#[cfg(test)]
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nmt_agent_utils::background_task::{BackgroundTaskKey, BackgroundTaskProvider};
use nmt_agent_utils::chat::{
    Event as SessionEvent, ForkAnchor, MessageImage, QuestionRequest, SendOutcome, SessionScope,
    SkillReference, SlashCommandInfo, SlashCommandOutcome, ThreadSettings,
};
use nmt_agent_utils::claude_code::sessions::RestoredTask;
use nmt_agent_utils::claude_code::stream_json;
use nmt_agent_utils::claude_code::workflows::{
    RestoredWorkflowRun, WorkflowRefreshRequest, WorkflowRefreshResult,
};
use nmt_agent_utils::codex::app_server;
use nmt_agent_utils::{AgentWorkspace, LaunchConfig, deepseek};
use nmt_i18n::i18n;
use serde_json::Value;
use tracing::trace;

use crate::composer::attachments::PendingAttachments;
use crate::profile::AgentKind;

/// The conversation a restarted backend should continue, qualified by the
/// harness that issued the id. Ids are only meaningful to the harness that
/// minted them, so a mismatched pair starts a fresh conversation instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryIdentity {
    pub(crate) kind: AgentKind,
    pub(crate) id: String,
}

impl RecoveryIdentity {
    pub fn new(kind: AgentKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

/// The pane's protocol session, one variant per agent kind. Every backend
/// shares the [`nmt_agent_utils::chat`] event vocabulary and method surface,
/// so the pane dispatches here and stays protocol-agnostic.
pub(crate) enum Backend {
    Codex(app_server::Session),
    Claude(stream_json::Session),
    DeepSeek(deepseek::Session),
    #[cfg(test)]
    Test(TestBackend),
}

pub(super) struct ConversationTitleRequest {
    pub(super) description: String,
    pub(super) provisional_title: String,
}

#[cfg(test)]
pub(crate) struct TestBackend {
    send_outcomes: VecDeque<SendOutcome>,
    slash_outcome: SlashCommandOutcome,
    commands: Vec<SlashCommandInfo>,
    /// Raised from `Drop` when a test needs to observe the moment the pane
    /// lets go of the session. A DeepSeek session's release is what can stop
    /// the shared host process, so when it happens is behavior of its own.
    released: Option<Arc<AtomicBool>>,
    recovery: Option<RecoveryIdentity>,
}

#[cfg(test)]
impl TestBackend {
    pub(crate) fn new(
        send_outcomes: impl IntoIterator<Item = SendOutcome>,
        slash_outcome: SlashCommandOutcome,
        commands: Vec<SlashCommandInfo>,
    ) -> Self {
        Self {
            send_outcomes: send_outcomes.into_iter().collect(),
            slash_outcome,
            commands,
            released: None,
            recovery: None,
        }
    }

    pub(crate) fn watch_release(mut self, released: Arc<AtomicBool>) -> Self {
        self.released = Some(released);
        self
    }

    pub(crate) fn with_recovery(mut self, kind: AgentKind, id: impl Into<String>) -> Self {
        self.recovery = Some(RecoveryIdentity::new(kind, id));
        self
    }
}

#[cfg(test)]
impl Drop for TestBackend {
    fn drop(&mut self) {
        if let Some(released) = &self.released {
            released.store(true, Ordering::SeqCst);
        }
    }
}

impl Backend {
    /// Start the harness process for `kind` and wrap it in the matching
    /// variant. Resume differs by harness — Codex asks the running app-server
    /// to reopen a thread, Claude Code takes a session id as a launch flag —
    /// so the caller passes an identity and this decides how to use it.
    pub(crate) fn spawn(
        kind: AgentKind,
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        recovery: Option<RecoveryIdentity>,
        deliver: impl Fn(Value) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let resume = recovery
            .filter(|identity| identity.kind == kind)
            .map(|identity| identity.id);
        // Harness stderr is forwarded at trace level: an agent turn emits tens of
        // thousands of these lines per second, and formatting plus writing them
        // costs enough main-thread time to drop frames.
        match kind {
            AgentKind::Codex => match resume {
                Some(thread_id) => app_server::Session::spawn_resuming(
                    launch,
                    host_catalog,
                    workspace,
                    thread_id,
                    true,
                    deliver,
                    |line| trace!("codex app-server: {line}"),
                ),
                None => {
                    app_server::Session::spawn(launch, host_catalog, workspace, deliver, |line| {
                        trace!("codex app-server: {line}")
                    })
                }
            }
            .map(Backend::Codex),
            AgentKind::Claude => {
                stream_json::Session::spawn(launch, workspace, resume, deliver, |line| {
                    trace!("claude: {line}")
                })
                .map(Backend::Claude)
            }
            // No process is started per tab here: the harness host is shared by
            // every DeepSeek tab, and this attaches a conversation to it,
            // starting it only if no tab holds one yet. `resume` is unused
            // because continuing an earlier conversation is not mapped yet.
            AgentKind::DeepSeek => deepseek::Session::create(launch, workspace, deliver)
                .map(Backend::DeepSeek)
                .map_err(|error| error.message().to_string()),
        }
    }

    pub(crate) fn process(&mut self, message: Value) -> Vec<SessionEvent> {
        match self {
            Backend::Codex(session) => session.process(message),
            Backend::Claude(session) => session.process(message),
            Backend::DeepSeek(session) => session.process(message),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    /// Send a message and the images it carries. Each harness takes them in
    /// its own shape: Codex reads files from disk, so the attachments are
    /// written under `scratch` first, while Claude Code and DeepSeek Harness
    /// take the bytes inline. A harness with no image input is sent the text
    /// alone, which is all a pane without `image_input` can have composed.
    pub(crate) fn send_user_message(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
        attachments: &PendingAttachments,
        scratch: &Path,
    ) -> SendOutcome {
        match self {
            Backend::Codex(session) => {
                let paths = write_attachments(attachments, scratch);
                session.send_user_message_with_skill(text, settings, skill, &paths)
            }
            Backend::Claude(session) => {
                session.send_user_message(text, settings, &inline_images(attachments))
            }
            // Skills are not mapped for DeepSeek, so a reference cannot reach
            // it and the prompt goes as the user wrote it.
            Backend::DeepSeek(session) => {
                session.send_user_message(text, &inline_images(attachments))
            }
            #[cfg(test)]
            Backend::Test(session) => session
                .send_outcomes
                .pop_front()
                .unwrap_or(SendOutcome::NotReady),
        }
    }

    /// Submit a message that gives an unnamed conversation its first title.
    /// Each provider owns the ordering its persistence model needs.
    pub(super) fn send_user_message_with_title(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
        attachments: &PendingAttachments,
        scratch: &Path,
        title: &ConversationTitleRequest,
    ) -> SendOutcome {
        match self {
            Backend::Codex(session) => {
                let paths = write_attachments(attachments, scratch);
                session.send_user_message_with_generated_title(
                    text,
                    settings,
                    skill,
                    &paths,
                    &title.provisional_title,
                )
            }
            Backend::Claude(session) => {
                let outcome =
                    session.send_user_message(text, settings, &inline_images(attachments));
                if matches!(outcome, SendOutcome::StartedTurn | SendOutcome::Steered) {
                    session.request_session_title(&title.description);
                }
                outcome
            }
            Backend::DeepSeek(session) => {
                session.send_user_message(text, &inline_images(attachments))
            }
            #[cfg(test)]
            Backend::Test(session) => session
                .send_outcomes
                .pop_front()
                .unwrap_or(SendOutcome::NotReady),
        }
    }

    pub(crate) fn adapter_commands(&self) -> Vec<SlashCommandInfo> {
        match self {
            Backend::Codex(_) => app_server::Session::adapter_commands(),
            Backend::Claude(_) => stream_json::Session::adapter_commands(),
            Backend::DeepSeek(_) => deepseek::Session::adapter_commands(),
            #[cfg(test)]
            Backend::Test(session) => session.commands.clone(),
        }
    }

    /// Drop one prompt the backend accepted but has not started. Answers
    /// whether the backend took the removal, so a row it has already claimed
    /// stays where the transcript is about to confirm it.
    pub(crate) fn remove_queued_prompt(&mut self, item_id: &str) -> bool {
        match self {
            Backend::DeepSeek(session) => session.remove_queued_prompt(item_id),
            // The other backends report no identity for their pending work, so
            // nothing here can name a message to remove.
            Backend::Codex(_) | Backend::Claude(_) => false,
            #[cfg(test)]
            Backend::Test(_) => false,
        }
    }

    /// Pin a title on the conversation, answering with the title the backend
    /// actually accepted after its own normalization.
    pub(crate) fn rename_conversation(&mut self, title: &str) -> Result<String, String> {
        match self {
            Backend::DeepSeek(session) => session.rename(title),
            Backend::Codex(_) | Backend::Claude(_) => {
                Err(i18n("agent-session-rename-unsupported").to_string())
            }
            #[cfg(test)]
            Backend::Test(_) => Err(i18n("agent-session-rename-unsupported").to_string()),
        }
    }

    /// Ask which prompts this conversation can be branched in front of. The
    /// answer arrives as [`Event::ForkCheckpoints`], so there is nothing to
    /// return here beyond whether the question could be put at all.
    pub(crate) fn request_fork_checkpoints(&mut self) -> bool {
        match self {
            Backend::Codex(session) => session.request_fork_checkpoints(),
            Backend::DeepSeek(session) => {
                session.request_fork_checkpoints();
                true
            }
            // Claude's history is a file this side reads directly, and its own
            // rewind picker is what reads it.
            Backend::Claude(_) => false,
            #[cfg(test)]
            Backend::Test(_) => false,
        }
    }

    /// Branch the conversation in front of `anchor` and move this session into
    /// the copy, leaving the conversation it branched from as it was.
    pub(crate) fn fork_conversation(&mut self, anchor: &ForkAnchor) -> Result<(), String> {
        match self {
            Backend::Codex(session) => session.fork_thread(anchor),
            Backend::DeepSeek(session) => session.fork(Some(anchor)),
            Backend::Claude(_) => Err(i18n("agent-session-fork-unsupported").to_string()),
            #[cfg(test)]
            Backend::Test(_) => Err(i18n("agent-session-fork-unsupported").to_string()),
        }
    }

    /// Ask the backend which earlier conversations mention a phrase. The
    /// answer arrives as a replacement history list, so there is nothing to
    /// return here.
    pub(crate) fn search_sessions(&mut self, query: &str) {
        match self {
            Backend::DeepSeek(session) => session.search_sessions(query),
            // `Capabilities::session_search` is what decides whether `/find`
            // is offered at all, so these arms are only reached by a caller
            // that skipped the question.
            Backend::Codex(_) | Backend::Claude(_) => {}
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    pub(crate) fn execute_slash_command(
        &mut self,
        name: &str,
        arguments: &str,
    ) -> SlashCommandOutcome {
        match self {
            Backend::Codex(session) => session.execute_slash_command(name, arguments),
            Backend::Claude(session) => session.execute_slash_command(name, arguments),
            Backend::DeepSeek(session) => session.execute_slash_command(name, arguments),
            #[cfg(test)]
            Backend::Test(session) => session.slash_outcome.clone(),
        }
    }

    pub(crate) fn rewind_files(&mut self, user_message_id: &str) -> SlashCommandOutcome {
        match self {
            Backend::Claude(session) => session.rewind_files(user_message_id),
            // `Capabilities::file_rewind` gates the command that leads here.
            // The rejection stays because it is the honest answer for a
            // harness with no such operation to run.
            Backend::Codex(_) | Backend::DeepSeek(_) => SlashCommandOutcome::Rejected {
                message: i18n("agent-session-file-rewind-claude-only").to_string(),
            },
            #[cfg(test)]
            Backend::Test(session) => session.slash_outcome.clone(),
        }
    }

    /// Ask the provider for fresher child-agent data. Adapters guard against
    /// overlapping requests themselves, so opening the panel repeatedly cannot
    /// queue duplicate discovery passes.
    pub(crate) fn refresh_background_tasks(&mut self) {
        match self {
            Backend::Codex(session) => session.refresh_background_tasks(),
            // Claude Code rebuilds tasks from session history rather than a
            // provider query, so there is nothing to re-request live.
            Backend::Claude(_) => {}
            Backend::DeepSeek(session) => session.refresh_background_tasks(),
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    /// Whether `key` names a task this backend published. A key carries the
    /// provider that minted it, and one harness's task ids mean nothing to
    /// another, so a key from elsewhere reaches no session at all.
    fn owns_task(&self, key: &BackgroundTaskKey) -> bool {
        let provider = match self {
            Backend::Codex(_) => BackgroundTaskProvider::Codex,
            Backend::Claude(_) => BackgroundTaskProvider::ClaudeCode,
            Backend::DeepSeek(_) => BackgroundTaskProvider::DeepSeek,
            #[cfg(test)]
            Backend::Test(_) => return false,
        };

        provider == key.provider
    }

    /// Ask the provider for one child's conversation. Codex reads the stored
    /// descendant thread; Claude Code reads the file the CLI wrote for that
    /// child, which is where a child's own turns live.
    pub(crate) fn load_background_task_transcript(
        &mut self,
        key: &BackgroundTaskKey,
        cwd: Option<&str>,
    ) -> Vec<SessionEvent> {
        if !self.owns_task(key) {
            return Vec::new();
        }

        match self {
            Backend::Codex(session) => session.load_background_task_transcript(&key.id),
            Backend::Claude(session) => session.load_background_task_transcript(&key.id, cwd),
            // The harness answers this one asynchronously, so the read starts
            // here and its result reaches the pane as an ordinary event.
            Backend::DeepSeek(session) => {
                session.load_background_task_transcript(&key.id);
                Vec::new()
            }
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    /// Stop one child agent without ending the parent's turn. Both harnesses
    /// can do it, by different means: a Codex child is a thread of its own and
    /// takes a thread-scoped `turn/interrupt`, while Claude Code registers each
    /// delegated agent under a task id that `stop_task` names.
    ///
    /// Returns whether the request went out.
    pub(crate) fn interrupt_background_task(&mut self, key: &BackgroundTaskKey) -> bool {
        if !self.owns_task(key) {
            return false;
        }

        match self {
            Backend::Codex(session) => session.interrupt_background_task(&key.id),
            Backend::Claude(session) => session.interrupt_background_task(key),
            Backend::DeepSeek(session) => session.interrupt_background_task(&key.id),
            #[cfg(test)]
            Backend::Test(_) => false,
        }
    }

    /// Take the sequence number a child-agent history read must not overwrite
    /// past. Live updates that land while the read runs keep their newer state.
    /// Only Claude Code rebuilds children from files, so Codex has no read to
    /// bracket and its sequence is unused.
    pub(crate) fn begin_task_restoration(&mut self) -> u64 {
        match self {
            Backend::Claude(session) => session.begin_task_restoration(),
            Backend::Codex(_) | Backend::DeepSeek(_) => 0,
            #[cfg(test)]
            Backend::Test(_) => 0,
        }
    }

    pub(crate) fn finish_task_restoration(
        &mut self,
        restored: Result<Vec<RestoredTask>, String>,
        starting_sequence: u64,
    ) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => {
                session.finish_task_restoration(restored, starting_sequence)
            }
            Backend::Codex(_) | Backend::DeepSeek(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    /// Continue an earlier conversation inside the running session. Returns
    /// whether the request reached a backend that can do it: Claude Code has no
    /// in-session resume and must respawn with the session id instead, so the
    /// caller keeps the recent-sessions list open and reports why.
    pub(crate) fn resume_thread(&mut self, thread_id: &str) -> bool {
        match self {
            Backend::Codex(session) => {
                session.resume_thread(thread_id);
                true
            }
            // The harness answers whether it attached, because a conversation
            // rooted in another directory is one this tab cannot adopt.
            Backend::DeepSeek(session) => session.resume_thread(thread_id),
            // `Capabilities::session_resume` sends this harness down the
            // respawn-and-replay path instead, so nothing routes a request
            // here to begin with.
            Backend::Claude(_) => false,
            #[cfg(test)]
            Backend::Test(_) => false,
        }
    }

    /// Ask for recent sessions over `scope`, replacing whatever an earlier
    /// scope produced. Only Codex lists over the protocol, and its server
    /// takes the scope as a filter it either applies or omits; Claude Code
    /// reads its own transcript directories, and the DeepSeek host scopes
    /// nothing by directory.
    pub(crate) fn request_history(&mut self, scope: SessionScope) {
        match self {
            Backend::Codex(session) => session.request_history(scope),
            Backend::Claude(_) | Backend::DeepSeek(_) => {}
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    /// Fetch the next page of recent sessions. Only Codex pages its history
    /// from the backend; Claude Code reads whole directories from disk, and the
    /// DeepSeek host answers with every visible session at once.
    pub(crate) fn request_more_history(&mut self) {
        match self {
            Backend::Codex(session) => session.request_more_history(),
            Backend::Claude(_) | Backend::DeepSeek(_) => {}
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    pub(crate) fn session_id(&self) -> Option<&str> {
        match self {
            Backend::Claude(session) => session.session_id(),
            Backend::DeepSeek(session) => session.session_id(),
            Backend::Codex(_) => None,
            #[cfg(test)]
            Backend::Test(_) => None,
        }
    }

    pub(crate) fn recovery_identity(&self) -> Option<RecoveryIdentity> {
        match self {
            Backend::Claude(session) => session
                .session_id()
                .map(|id| RecoveryIdentity::new(AgentKind::Claude, id)),
            Backend::Codex(session) => session
                .thread_id()
                .map(|id| RecoveryIdentity::new(AgentKind::Codex, id)),
            // The id names the conversation on the harness host, which is worth
            // reporting even though resuming into it is not mapped yet.
            Backend::DeepSeek(session) => session
                .session_id()
                .map(|id| RecoveryIdentity::new(AgentKind::DeepSeek, id)),
            #[cfg(test)]
            Backend::Test(session) => session.recovery.clone(),
        }
    }

    /// Name the conversation this backend holds when its provider stores a
    /// user-authored title of its own.
    pub(crate) fn rename_session(&mut self, title: &str) {
        match self {
            Backend::Claude(session) => session.rename_session(title),
            Backend::Codex(session) => session.rename_thread(title),
            Backend::DeepSeek(_) => {}
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    pub(crate) fn cancel_title_generation(&mut self) {
        if let Backend::Codex(session) = self {
            session.cancel_title_generation();
        }
    }

    pub(crate) fn has_active_operation(&self) -> bool {
        match self {
            Backend::Claude(session) => session.has_active_operation(),
            Backend::Codex(session) => session.has_active_operation(),
            Backend::DeepSeek(session) => session.has_active_operation(),
            #[cfg(test)]
            Backend::Test(_) => false,
        }
    }

    pub(crate) fn shutdown(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        match self {
            Backend::Claude(session) => session.shutdown(timeout, force),
            Backend::Codex(session) => session.shutdown(timeout, force),
            // Dropping this session releases its hold on the shared host, and
            // the last tab to let go stops it. Nothing here has to wait.
            Backend::DeepSeek(_) => Ok(()),
            #[cfg(test)]
            Backend::Test(_) => Ok(()),
        }
    }

    pub(crate) fn process_exit(&mut self) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => session.process_exit(),
            Backend::Codex(_) | Backend::DeepSeek(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    pub(crate) fn interrupt(&mut self) {
        match self {
            Backend::Codex(session) => session.interrupt(),
            Backend::Claude(session) => session.interrupt(),
            Backend::DeepSeek(session) => session.interrupt(),
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    pub(crate) fn respond_approval(&mut self, decision: &str) {
        match self {
            Backend::Codex(session) => session.respond_approval(decision),
            Backend::Claude(session) => session.respond_approval(decision),
            Backend::DeepSeek(session) => session.respond_approval(decision),
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    /// Ask for one workflow member's conversation. Only a harness that reports
    /// its runs live answers this; the disk-backed one reads a stored record
    /// through its own refresh path instead.
    pub(crate) fn request_workflow_agent_transcript(&mut self, task_id: &str, agent_id: &str) {
        match self {
            Backend::DeepSeek(session) => {
                session.request_workflow_agent_transcript(task_id, agent_id)
            }
            Backend::Codex(_) | Backend::Claude(_) => {}
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }

    /// What each still-running workflow run needs read on the next refresh
    /// tick. Only Claude reports workflows, so Codex has nothing to read.
    pub(crate) fn workflow_refresh_requests(&self) -> Vec<WorkflowRefreshRequest> {
        match self {
            Backend::Claude(session) => session.workflow_refresh_requests(),
            Backend::Codex(_) | Backend::DeepSeek(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    pub(crate) fn apply_workflow_refresh(
        &mut self,
        result: WorkflowRefreshResult,
    ) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => session.apply_workflow_refresh(result),
            Backend::Codex(_) | Backend::DeepSeek(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    pub(crate) fn restore_workflows(
        &mut self,
        restored: Vec<RestoredWorkflowRun>,
    ) -> Vec<SessionEvent> {
        match self {
            Backend::Claude(session) => session.restore_workflows(restored),
            Backend::Codex(_) | Backend::DeepSeek(_) => Vec::new(),
            #[cfg(test)]
            Backend::Test(_) => Vec::new(),
        }
    }

    /// Point the session at another model. Only DeepSeek applies a pick as its
    /// own request: Codex carries thread settings as overrides on the next
    /// turn, and Claude bakes the model into the launch.
    pub(crate) fn select_model(&mut self, model: &str, effort: Option<&str>) -> Result<(), String> {
        match self {
            Backend::DeepSeek(session) => session.select_model(model, effort),
            Backend::Codex(_) | Backend::Claude(_) => Ok(()),
            #[cfg(test)]
            Backend::Test(_) => Ok(()),
        }
    }

    /// Rebuild the conversation's agent from another composition. Only DeepSeek
    /// composes an agent from a preset at all; the other two launch one CLI
    /// whose capabilities are fixed for the life of the process.
    pub(crate) fn select_agent_preset(&mut self, preset: &str) -> Result<(), String> {
        match self {
            Backend::DeepSeek(session) => session.select_agent_preset(preset),
            Backend::Codex(_) | Backend::Claude(_) => Ok(()),
            #[cfg(test)]
            Backend::Test(_) => Ok(()),
        }
    }

    /// What the session is actually set to, for restoring the pickers after a
    /// refused pick.
    pub(crate) fn selection(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Backend::DeepSeek(session) => session.selection(),
            Backend::Codex(_) | Backend::Claude(_) => (None, None),
            #[cfg(test)]
            Backend::Test(_) => (None, None),
        }
    }

    pub(crate) fn restore_question_requests(&mut self, requests: Vec<QuestionRequest>) {
        if let Backend::Codex(session) = self {
            session.restore_question_requests(requests);
        }
    }

    pub(crate) fn respond_input(
        &mut self,
        id: &str,
        answers: Option<Vec<Vec<String>>>,
        settings: &ThreadSettings,
    ) -> Result<(), String> {
        match self {
            Backend::Codex(session) => session.respond_input(id, answers, settings),
            _ => Err("This session cannot answer that question".to_string()),
        }
    }

    /// Answer a provider's selection request using its original response format.
    pub(crate) fn respond_questions(&mut self, answers: Option<Vec<Vec<String>>>) {
        match self {
            Backend::Claude(session) => session.respond_questions(answers),
            Backend::DeepSeek(session) => session.respond_questions(answers),
            Backend::Codex(_) => {}
            #[cfg(test)]
            Backend::Test(_) => {}
        }
    }
}

/// The attachments in the form a harness that takes bytes inline wants them. Nothing reaches disk on this path, so there is no partial outcome to
/// report: every attachment the composer holds travels with the message.
fn inline_images(attachments: &PendingAttachments) -> Vec<MessageImage> {
    attachments
        .iter()
        .map(|attachment| MessageImage {
            bytes: attachment.bytes().to_vec(),
            media_type: attachment.format().mime_type().to_string(),
        })
        .collect()
}

/// Write each attachment into `scratch`, returning the paths that could be
/// written. A file that cannot be written is left out rather than failing the
/// message: the text and the images that did land are still worth sending.
fn write_attachments(attachments: &PendingAttachments, scratch: &Path) -> Vec<PathBuf> {
    if attachments.is_empty() || fs::create_dir_all(scratch).is_err() {
        return Vec::new();
    }

    attachments
        .iter()
        .enumerate()
        .filter_map(|(index, attachment)| {
            // Named by position within the message, and rewritten on every
            // send, so a pane's scratch directory never grows past one
            // message's worth of files.
            let path = scratch.join(format!("image-{}.png", index + 1));

            fs::write(&path, attachment.bytes()).ok().map(|()| path)
        })
        .collect()
}
