#[cfg(test)]
#[path = "children_tests.rs"]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskTranscriptState,
    BackgroundTaskTranscriptUpdate, MAX_TRANSCRIPT_ITEMS,
};
use crate::session::{AgentKind, RecoveryIdentity};
use crate::transcript::conversation::ConversationState;

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
    pub transcripts: HashMap<BackgroundTaskKey, ChildTranscript>,

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

/// Retained child content is shared with readers without copying its items.
#[derive(Default)]
pub struct ChildTranscript {
    pub conversation: Rc<RefCell<ConversationState>>,
    state: BackgroundTaskTranscriptState,
    dropped: usize,
}

impl ChildTranscript {
    pub fn state(&self) -> &BackgroundTaskTranscriptState {
        &self.state
    }

    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn apply(&mut self, update: BackgroundTaskTranscriptUpdate) -> bool {
        let mut changed = false;

        if let Some(state) = update.state
            && self.state != state
        {
            self.state = state;
            changed = true;
        }

        let mut conversation = self.conversation.borrow_mut();

        if update.restore && !conversation.content.entries().is_empty() {
            return changed;
        }

        if update.replace || update.restore {
            if conversation
                .content
                .entries()
                .iter()
                .map(|entry| &entry.item)
                .eq(update.items.iter())
            {
                return changed;
            }

            conversation.clear();

            self.dropped = 0;
            changed = true;
        }

        for item in update.items {
            if item
                .id()
                .is_some_and(|id| conversation.content.contains_item(id))
            {
                conversation.merge_completed(&item);
            } else {
                conversation.push(0, item, Vec::new());
            }

            changed = true;
        }

        let dropped = conversation.retain_last(MAX_TRANSCRIPT_ITEMS);

        if dropped > 0 {
            self.dropped += dropped;
        }

        changed
    }
}
