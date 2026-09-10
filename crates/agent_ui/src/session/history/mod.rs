//! The recent-conversation list: which conversations a tab offers to reopen,
//! where that list comes from, and what reopening one does.
//!
//! A harness that lists over the protocol is asked; one that keeps its
//! transcripts on disk is read here instead, which is why the list has a
//! loading shape of its own rather than simply arriving.

use std::time::Duration;

use gpui::Context;
use nmt_agent::chat::{SessionScope, SessionSummary};
use nmt_agent::claude_code::sessions;
use nmt_agent::session::restore::{ReplayLoaded, ResumeStart, SettingsSeed};
use nmt_i18n::i18n;

use crate::capabilities::AgentCapabilities as _;
use crate::composer::CommandFeedbackKind;
use crate::{AgentPane, AgentPaneEvent, RecentSessionsMode, SessionHistoryUi};

#[cfg(test)]
mod restore_tests;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilesystemHistoryRequest {
    id: u64,
    scope: SessionScope,
    cwd: Option<String>,
    epoch: u64,
}

enum CountPublication {
    Stale,
    Empty,
    LoadRows,
}

impl SessionHistoryUi {
    // Disk reads may finish after their view has been replaced. Retiring the
    // request also removes its placeholders without waiting for that work.
    pub(super) fn invalidate_filesystem_history(&mut self) {
        self.filesystem_request = None;
        self.pending = None;
    }

    fn begin_filesystem_history(
        &mut self,
        cwd: Option<String>,
        epoch: u64,
    ) -> FilesystemHistoryRequest {
        self.invalidate_filesystem_history();
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .expect("history request id exhausted");

        let request = FilesystemHistoryRequest {
            id: self.next_request_id,
            scope: self.scope,
            cwd,
            epoch,
        };

        self.filesystem_request = Some(request.clone());

        request
    }

    fn owns_filesystem_request(
        &self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
    ) -> bool {
        self.filesystem_request.as_ref() == Some(request)
            && self.scope == request.scope
            && request.cwd.as_deref() == cwd
            && request.epoch == epoch
    }

    fn publish_filesystem_count(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
        count: usize,
    ) -> CountPublication {
        if !self.owns_filesystem_request(request, cwd, epoch) {
            return CountPublication::Stale;
        }

        if count == 0 {
            self.sessions.clear();
            self.selected = 0;
            self.invalidate_filesystem_history();

            CountPublication::Empty
        } else {
            self.pending = Some(count);
            CountPublication::LoadRows
        }
    }

    fn publish_filesystem_rows(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
        sessions: Vec<SessionSummary>,
    ) -> bool {
        if !self.owns_filesystem_request(request, cwd, epoch) {
            return false;
        }

        self.sessions = sessions;
        self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
        self.invalidate_filesystem_history();

        true
    }
}

/// The filesystem history a scope covers. Only a backend that reads its own
/// transcripts takes this route; one that lists over the protocol asks its
/// server for the scope instead.
fn count_scoped_sessions(scope: SessionScope, cwd: Option<&str>) -> usize {
    match scope {
        SessionScope::CurrentDirectory => sessions::count_sessions(cwd),
        SessionScope::AllDirectories => sessions::count_all_sessions(),
    }
}

fn list_scoped_sessions(scope: SessionScope, cwd: Option<&str>) -> Vec<SessionSummary> {
    match scope {
        SessionScope::CurrentDirectory => sessions::list_sessions(cwd),
        SessionScope::AllDirectories => sessions::list_all_sessions(),
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
        self.history_ui.scope = match self.history_ui.scope {
            SessionScope::CurrentDirectory => SessionScope::AllDirectories,
            SessionScope::AllDirectories => SessionScope::CurrentDirectory,
        };
        self.history_ui.sessions.clear();
        self.history_ui.showing_search = false;
        self.history_ui.selected = 0;

        if let Some(session) = self.runtime.backend_mut() {
            session.request_history(self.history_ui.scope);
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
        let scope = self.history_ui.scope;
        let request = self
            .history_ui
            .begin_filesystem_history(cwd.clone(), self.runtime.epoch());

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
                        this.runtime.epoch(),
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
                    this.runtime.epoch(),
                    sessions,
                ) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn seed_restored_settings(&mut self, seed: SettingsSeed) {
        let (defaults, reviewer) = match seed {
            SettingsSeed::Defaults => (true, false),
            SettingsSeed::Reviewer => (false, true),
            SettingsSeed::None => (false, false),
        };
        self.controls.seed_thread_defaults = defaults;
        self.controls.seed_approval_reviewer = reviewer;
    }

    /// Keep the displayed conversation until the replacement supplies its replay.
    pub(crate) fn resume_session(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(summary) = self.history_ui.sessions.get(index) else {
            return;
        };

        // Both operations replace the conversation; a visible history list
        // must not start a resume while a branch picker or file step owns it.
        if self.history_ui.mode == RecentSessionsMode::Loading || self.branch.holds_composer() {
            return;
        }

        let cwd = self.cwd();
        let request =
            match self
                .restore
                .begin(&mut self.runtime, self.kind, summary, cwd.as_deref())
            {
                ResumeStart::Busy => return,
                ResumeStart::Elsewhere { cwd, session_id } => {
                    self.history_ui.selected = index;
                    cx.emit(AgentPaneEvent::ResumeElsewhere { cwd, session_id });
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

        cx.spawn(async move |this, cx| {
            let (request, replay) = cx
                .background_executor()
                .spawn(async move {
                    let replay = request.load();
                    (request, replay)
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                let cwd = this.cwd();
                match this
                    .restore
                    .loaded(&mut this.runtime, request, cwd.as_deref(), replay)
                {
                    ReplayLoaded::Stale => {}
                    ReplayLoaded::Cancelled => {
                        this.history_ui.mode = RecentSessionsMode::Open;
                        this.palette.feedback = None;
                        cx.notify();
                    }
                    ReplayLoaded::Failed(message) => {
                        this.history_ui.mode = RecentSessionsMode::Open;
                        this.palette.set_feedback(
                            CommandFeedbackKind::Error,
                            i18n("agent-session-open-failed").replace("{error}", &message),
                            cx,
                        );
                    }
                    ReplayLoaded::Restart(identity) => {
                        this.start_session_with_options(
                            Some(identity),
                            false,
                            |this, started, _| {
                                if !started {
                                    this.restore.failed(&mut this.runtime);
                                    this.history_ui.mode = RecentSessionsMode::Open;
                                }
                            },
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }
}
