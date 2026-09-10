use std::collections::HashMap;

use crate::background_task::{BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskTranscript};
use crate::session::{AgentKind, RecoveryIdentity};
/// Child-agent activity the provider adapter reports for this conversation.
#[derive(Default)]
pub struct ChildAgents {
    /// Latest child-agent snapshot published by the provider adapter. The
    /// adapter owns child lifecycle; the pane keeps only this replacement
    /// copy so the right-side view never maintains a second mutable registry.
    pub background_tasks: Option<BackgroundTaskSnapshot>,
    /// Each child's own conversation, accumulated here rather than in the
    /// adapter so live activity is retained once and the retention bound
    /// applies to what is actually shown.
    pub transcripts: HashMap<BackgroundTaskKey, BackgroundTaskTranscript>,
    /// Claude session id whose child agents were already restored from
    /// history. Ready fires again during first-turn initialization, so the
    /// read happens once per conversation rather than once per confirmation.
    pub restored_session: Option<String>,
}

/// Show a task snapshot only against the parent session it was produced for.
/// Provider adapters publish snapshots asynchronously, so a snapshot can still
/// be held when the pane has already moved to another session or has no
/// session id yet; in both cases the view must render nothing rather than
/// another conversation's children.
pub fn scoped_background_tasks<'a>(
    parent: Option<&BackgroundTaskKey>,
    snapshot: Option<&'a BackgroundTaskSnapshot>,
) -> Option<&'a BackgroundTaskSnapshot> {
    let parent = parent?;
    let snapshot = snapshot?;
    (&snapshot.parent_session == parent).then_some(snapshot)
}

impl ChildAgents {
    pub fn claim_restore(&mut self, session_id: &str) -> bool {
        if self.restored_session.as_deref() == Some(session_id) {
            return false;
        }
        self.restored_session = Some(session_id.to_owned());
        true
    }

    pub fn parent(identity: RecoveryIdentity) -> Option<BackgroundTaskKey> {
        Some(match identity.kind {
            AgentKind::Codex => BackgroundTaskKey::codex(identity.id),
            AgentKind::Claude => BackgroundTaskKey::claude_code(identity.id),
            AgentKind::DeepSeek => return None,
        })
    }
}
