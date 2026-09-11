//! The recent-conversation list: which conversations a tab offers to reopen,
//! where that list comes from, and what reopening one does.
//!
//! A harness that lists over the protocol is asked; one that keeps its
//! transcripts on disk is read here instead, which is why the list has a
//! loading shape of its own rather than simply arriving.

use std::time::Duration;

use gpui::Context;
use nmt_agent::chat::{SessionScope, SessionSummary};
use nmt_agent::session::restore::{ResumeStart, SettingsSeed};
use nmt_i18n::i18n;

use crate::capabilities::AgentCapabilities as _;
use crate::composer::CommandFeedbackKind;
use crate::{AgentPane, AgentPaneEvent, RecentSessionsMode, SessionHistoryUi};

#[cfg(test)]
mod restore_tests;
#[cfg(test)]
mod tests;

pub(crate) use nmt_agent::session::history::FilesystemHistoryRequest;
use nmt_agent::session::history::{CountPublication, count_scoped_sessions, list_scoped_sessions};

impl SessionHistoryUi {
    pub(super) fn invalidate_filesystem_history(&mut self) {
        self.data.invalidate_filesystem_history();
    }

    fn begin_filesystem_history(
        &mut self,
        cwd: Option<String>,
        epoch: u64,
    ) -> FilesystemHistoryRequest {
        self.data.begin_filesystem_history(cwd, epoch)
    }

    fn publish_filesystem_count(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
        count: usize,
    ) -> CountPublication {
        let result = self
            .data
            .publish_filesystem_count(request, cwd, epoch, count);

        if matches!(result, CountPublication::Empty) {
            self.selected = 0;
        }

        result
    }

    fn publish_filesystem_rows(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
        rows: Vec<SessionSummary>,
    ) -> bool {
        if !self.data.publish_filesystem_rows(request, cwd, epoch, rows) {
            return false;
        }

        self.selected = self
            .selected
            .min(self.data.sessions.len().saturating_sub(1));

        true
    }
}

impl AgentPane {
    /// Widen the session list to every directory, or narrow it back to this
    /// tab's. The rows on screen answered the previous scope, so they go; the
    /// reload republishes what the new one covers. A backend that lists over
    /// the protocol is asked again, one that reads its own transcripts is
    /// rescanned.
    pub(crate) fn toggle_history_scope(&mut self, cx: &mut Context<Self>) {
        self.history_ui.invalidate_filesystem_history();

        self.history_ui.data.scope = match self.history_ui.data.scope {
            SessionScope::CurrentDirectory => SessionScope::AllDirectories,
            SessionScope::AllDirectories => SessionScope::CurrentDirectory,
        };

        self.history_ui.data.sessions.clear();
        self.history_ui.data.showing_search = false;
        self.history_ui.selected = 0;

        if let Some(session) = self.session.borrow_mut().runtime.backend_mut() {
            session.request_history(self.history_ui.data.scope);
        }

        self.load_filesystem_history(cx);

        cx.notify();
    }

    /// History read from the CLI's transcript directory, for a harness that
    /// does not deliver it over the protocol as `Event::History`. Two passes,
    /// both off-thread: a cheap count first, so the list can reserve its final
    /// height with placeholder rows, then title parsing, which swaps in the
    /// real rows.
    pub(super) fn load_filesystem_history(&mut self, cx: &mut Context<Self>) {
        if !self.kind.caps().filesystem_session_history {
            return;
        }

        let cwd = self.cwd();
        let scope = self.history_ui.data.scope;

        let request = self
            .history_ui
            .begin_filesystem_history(cwd.clone(), self.session.borrow().runtime.epoch());

        cx.notify();

        cx.spawn(async move |this, cx| {
            let count_cwd = cwd.clone();

            let count = cx
                .background_executor()
                .spawn(async move { count_scoped_sessions(scope, count_cwd.as_deref()) })
                .await;

            let proceed = this
                .update(cx, |this, cx| {
                    let cwd = this.cwd();

                    match this.history_ui.publish_filesystem_count(
                        &request,
                        cwd.as_deref(),
                        this.session.borrow().runtime.epoch(),
                        count,
                    ) {
                        CountPublication::Stale => false,

                        CountPublication::Empty => {
                            cx.notify();

                            false
                        }

                        CountPublication::LoadRows => {
                            cx.notify();

                            true
                        }
                    }
                })
                .unwrap_or(false);

            if !proceed {
                return;
            }

            // Title parsing races a short hold: on a warm SSD it finishes
            // within a frame, so without the hold the skeleton rows would
            // never be visible and the swap would read as a flicker.
            let load = cx
                .background_executor()
                .spawn(async move { list_scoped_sessions(scope, cwd.as_deref()) });

            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;

            let sessions = load.await;

            let _ = this.update(cx, |this, cx| {
                let cwd = this.cwd();

                if this.history_ui.publish_filesystem_rows(
                    &request,
                    cwd.as_deref(),
                    this.session.borrow().runtime.epoch(),
                    sessions,
                ) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn seed_restored_settings(&mut self, seed: SettingsSeed) {
        if !self.binding.is_current() {
            return;
        }

        let (defaults, reviewer) = match seed {
            SettingsSeed::Defaults => (true, false),
            SettingsSeed::Reviewer => (false, true),
            SettingsSeed::None => (false, false),
        };

        self.session.borrow_mut().controls.seed_thread_defaults = defaults;
        self.session.borrow_mut().controls.seed_approval_reviewer = reviewer;
    }

    /// Keep the displayed conversation until the replacement supplies its replay.
    pub(crate) fn resume_session(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        let Some(summary) = self.history_ui.data.sessions.get(index) else {
            return;
        };

        // Both operations replace the conversation; a visible history list
        // must not start a resume while a branch picker or file step owns it.
        if self.history_ui.mode == RecentSessionsMode::Loading
            || self.session.borrow().branch.holds_composer()
        {
            return;
        }

        let cwd = self.cwd();

        let outcome = {
            let mut guard = self.session.borrow_mut();
            let state = &mut *guard;

            state
                .restore
                .begin(&mut state.runtime, self.kind, summary, cwd.as_deref())
        };

        let request = match outcome {
            ResumeStart::Busy => return,

            ResumeStart::Elsewhere { cwd, session_id } => {
                self.history_ui.selected = index;

                self.emit_event(AgentPaneEvent::ResumeElsewhere { cwd, session_id }, cx);

                cx.notify();

                return;
            }

            ResumeStart::Rejected => {
                self.history_ui.mode = RecentSessionsMode::Open;
                self.history_ui.selected = index;

                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-session-codex-recent-not-ready").to_string(),
                    cx,
                );

                return;
            }

            ResumeStart::Requested => {
                self.seed_restored_settings(SettingsSeed::resumed(self.kind));

                None
            }

            ResumeStart::ReadReplay(request) => Some(request),
        };

        self.history_ui.mode = RecentSessionsMode::Loading;
        self.history_ui.selected = index;

        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            i18n("agent-session-opening-recent").to_string(),
            cx,
        );

        let Some(request) = request else {
            return;
        };

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.read_resume(request, cx));
        }
    }
}
