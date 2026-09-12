//! Child agents this conversation has delegated to: what the pane knows about
//! them, and the questions the view asks about that.
//!
//! A snapshot belongs to the conversation that spawned it, so everything here
//! is scoped to the session's own key and a snapshot from a replaced
//! conversation reaches nothing.

use std::cell::Ref;

use gpui::Context;
use nmt_agent::background_task::{BackgroundTaskKey, BackgroundTaskSnapshot};
use nmt_agent::session::children::ChildTranscript;
#[cfg(test)]
pub(super) use nmt_agent::session::children::scoped_background_tasks;

use crate::agent_tab::AgentPane;
use crate::agent_tab::execution::ChildReader;

impl AgentPane {
    pub fn refresh_background_tasks(&mut self) {
        self.session.borrow_mut().refresh_background_tasks();
    }

    /// Provider-qualified identity of the parent session child tasks belong to.
    /// `None` until the backend reports a thread or session id, which is what
    /// disables the title-bar `Background Tasks` button.
    pub fn background_task_parent(&self) -> Option<BackgroundTaskKey> {
        self.session.borrow().background_task_parent()
    }

    /// Ask the provider for one child's conversation. A provider that already
    /// has it, or that streams it live, does no work here.
    pub fn watch_background_task(
        &mut self,
        key: &BackgroundTaskKey,
        cx: &mut Context<Self>,
    ) -> Option<ChildReader> {
        self.host
            .upgrade()?
            .update(cx, |host, cx| host.watch_child(key, cx))
    }

    /// Stop one child agent, leaving this tab's own turn running. Reports
    /// whether the request was accepted, so the view can say so when a child
    /// turns out not to be stoppable after all — the snapshot a row was drawn
    /// from can be a moment behind the child finishing on its own.
    pub fn interrupt_background_task(&mut self, key: &BackgroundTaskKey) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        self.session.borrow_mut().interrupt_background_task(key)
    }

    /// One child's conversation, only while the pane still holds the session
    /// that child belongs to.
    pub fn background_task_transcript(
        &self,
        key: &BackgroundTaskKey,
    ) -> Option<Ref<'_, ChildTranscript>> {
        Ref::filter_map(self.session.borrow(), |session| {
            session.background_task_transcript(key)
        })
        .ok()
    }

    /// The latest snapshot, only while it still describes the session the pane
    /// currently holds. A snapshot left over from a replaced session is hidden
    /// rather than shown against the new parent.
    pub fn background_tasks(&self) -> Option<Ref<'_, BackgroundTaskSnapshot>> {
        Ref::filter_map(self.session.borrow(), |session| session.background_tasks()).ok()
    }

    /// Child agents of this tab the provider currently reports as active.
    pub fn running_background_tasks(&self) -> usize {
        self.background_tasks()
            .map(|snapshot| snapshot.active_count())
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
