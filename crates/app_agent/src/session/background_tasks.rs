//! Child agents this conversation has delegated to: what the pane knows about
//! them, and the questions the view asks about that.
//!
//! A snapshot belongs to the conversation that spawned it, so everything here
//! is scoped to the session's own key and a snapshot from a replaced
//! conversation reaches nothing.

use gpui::Context;
use nmt_agent_utils::background_task::{
    BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskTranscript,
};
use nmt_agent_utils::claude_code::sessions;

use crate::AgentPane;
use crate::profile::AgentKind;

/// Show a task snapshot only against the parent session it was produced for.
/// Provider adapters publish snapshots asynchronously, so a snapshot can still
/// be held when the pane has already moved to another session or has no
/// session id yet; in both cases the view must render nothing rather than
/// another conversation's children.
pub(super) fn scoped_background_tasks<'a>(
    parent: Option<&BackgroundTaskKey>,
    snapshot: Option<&'a BackgroundTaskSnapshot>,
) -> Option<&'a BackgroundTaskSnapshot> {
    let parent = parent?;
    let snapshot = snapshot?;
    (&snapshot.parent_session == parent).then_some(snapshot)
}

impl AgentPane {
    /// Rebuild Claude child agents from the session's persisted history. The
    /// read runs on a background thread and its failure never blocks the
    /// parent transcript or composer.
    pub(crate) fn restore_background_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self
            .runtime
            .backend
            .as_ref()
            .and_then(|session| session.session_id())
            .map(str::to_owned)
        else {
            return;
        };
        if self.children.restored_session.as_deref() == Some(session_id.as_str()) {
            return;
        }
        self.children.restored_session = Some(session_id.clone());

        let Some(session) = self.runtime.backend.as_mut() else {
            return;
        };
        // Captured before the read starts so live updates that land while it
        // runs keep their newer state.
        let starting_sequence = session.begin_task_restoration();
        let cwd = self.cwd();
        let epoch = self.runtime.epoch;

        cx.spawn(async move |this, cx| {
            let restored = cx
                .background_executor()
                .spawn(async move { sessions::load_task_history(cwd.as_deref(), &session_id) })
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.runtime.epoch != epoch {
                    return;
                }
                let Some(session) = this.runtime.backend.as_mut() else {
                    return;
                };
                for event in session.finish_task_restoration(restored, starting_sequence) {
                    this.apply_event(event, cx);
                }
            });
        })
        .detach();
    }

    pub fn refresh_background_tasks(&mut self) {
        if let Some(session) = self.runtime.backend.as_mut() {
            session.refresh_background_tasks();
        }
    }

    /// Provider-qualified identity of the parent session child tasks belong to.
    /// `None` until the backend reports a thread or session id, which is what
    /// disables the title-bar `Background Tasks` button.
    pub fn background_task_parent(&self) -> Option<BackgroundTaskKey> {
        let identity = self.runtime.backend.as_ref()?.recovery_identity()?;
        Some(match identity.kind {
            AgentKind::Codex => BackgroundTaskKey::codex(identity.id),
            AgentKind::Claude => BackgroundTaskKey::claude_code(identity.id),
            // Child agents are not mapped for DeepSeek yet, so there is no
            // parent to name and the Background Tasks button stays disabled.
            AgentKind::DeepSeek => return None,
        })
    }

    /// Ask the provider for one child's conversation. A provider that already
    /// has it, or that streams it live, does no work here.
    pub fn load_background_task_transcript(
        &mut self,
        key: &BackgroundTaskKey,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.cwd();
        let Some(session) = self.runtime.backend.as_mut() else {
            return;
        };
        for event in session.load_background_task_transcript(key, cwd.as_deref()) {
            self.apply_event(event, cx);
        }
    }

    /// Stop one child agent, leaving this tab's own turn running. Reports
    /// whether the request was accepted, so the view can say so when a child
    /// turns out not to be stoppable after all — the snapshot a row was drawn
    /// from can be a moment behind the child finishing on its own.
    pub fn interrupt_background_task(&mut self, key: &BackgroundTaskKey) -> bool {
        self.runtime
            .backend
            .as_mut()
            .is_some_and(|session| session.interrupt_background_task(key))
    }

    /// One child's conversation, only while the pane still holds the session
    /// that child belongs to.
    pub fn background_task_transcript(
        &self,
        key: &BackgroundTaskKey,
    ) -> Option<&BackgroundTaskTranscript> {
        self.background_tasks()?;
        self.children.transcripts.get(key)
    }

    /// The latest snapshot, only while it still describes the session the pane
    /// currently holds. A snapshot left over from a replaced session is hidden
    /// rather than shown against the new parent.
    pub fn background_tasks(&self) -> Option<&BackgroundTaskSnapshot> {
        scoped_background_tasks(
            self.background_task_parent().as_ref(),
            self.children.background_tasks.as_ref(),
        )
    }

    /// Child agents of this tab the provider currently reports as active.
    pub fn running_background_tasks(&self) -> usize {
        self.background_tasks()
            .map(BackgroundTaskSnapshot::active_count)
            .unwrap_or(0)
    }

    /// Child agents this tab has, running and finished alike. A finished child
    /// is still something to open the view for, so the chrome asks for this
    /// rather than the running count when deciding to offer the control.
    pub fn background_task_count(&self) -> usize {
        self.background_tasks()
            .map(|tasks| tasks.tasks.len())
            .unwrap_or(0)
    }
}
