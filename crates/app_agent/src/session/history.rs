//! The recent-conversation list: which conversations a tab offers to reopen,
//! where that list comes from, and what reopening one does.
//!
//! A harness that lists over the protocol is asked; one that keeps its
//! transcripts on disk is read here instead, which is why the list has a
//! loading shape of its own rather than simply arriving.

use std::time::Duration;

use gpui::Context;
use nmt_agent_utils::chat::{SessionScope, SessionSummary};
use nmt_agent_utils::claude_code::sessions;
use nmt_i18n::i18n;

use crate::composer::CommandFeedbackKind;
use crate::session::backend::RecoveryIdentity;
use crate::session::{Status, directories_match};
use crate::{AgentPane, AgentPaneEvent, RecentSessionsMode};

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
        self.history_ui.scope = match self.history_ui.scope {
            SessionScope::CurrentDirectory => SessionScope::AllDirectories,
            SessionScope::AllDirectories => SessionScope::CurrentDirectory,
        };
        self.history_ui.sessions.clear();
        self.history_ui.showing_search = false;
        self.history_ui.selected = 0;

        if let Some(session) = self.runtime.backend.as_mut() {
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

        cx.spawn(async move |this, cx| {
            let count_cwd = cwd.clone();
            let count = cx
                .background_executor()
                .spawn(async move { count_scoped_sessions(scope, count_cwd.as_deref()) })
                .await;

            let proceed = this
                .update(cx, |this, cx| {
                    this.history_ui.pending = Some(count);
                    cx.notify();

                    count > 0
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
                this.history_ui.sessions = sessions;
                this.history_ui.pending = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// Resume the picked history entry without discarding the visible
    /// conversation until the target confirms it can be opened.
    pub(crate) fn resume_session(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(summary) = self.history_ui.sessions.get(index) else {
            return;
        };
        let id = summary.id.clone();
        let elsewhere = summary
            .cwd
            .clone()
            .filter(|cwd| !directories_match(Some(cwd.as_str()), self.cwd().as_deref()));

        if self.history_ui.mode == RecentSessionsMode::Loading {
            return;
        }

        // A conversation belongs to the directory it ran in. Continuing one
        // from elsewhere in this tab would either fail to find it or run it
        // against the wrong tree, so it opens where it worked instead.
        if let Some(cwd) = elsewhere {
            self.history_ui.selected = index;
            cx.emit(AgentPaneEvent::ResumeElsewhere {
                cwd,
                session_id: id,
            });
            cx.notify();
            return;
        }

        let previous_status = self.runtime.status;
        self.history_ui.mode = RecentSessionsMode::Loading;
        self.history_ui.selected = index;
        self.history_ui.pending_resume_replay = None;
        self.runtime.status = Status::Starting;
        // A backend that replays the resumed conversation's controls owns them;
        // otherwise they stay local profile preferences. The reviewer is seeded
        // separately because a backend can replay the rest without it.
        let caps = self.kind.caps();
        self.controls.seed_thread_defaults = !caps.resume_restores_thread_settings;
        self.controls.seed_approval_reviewer =
            caps.resume_restores_thread_settings && !caps.resume_restores_approval_reviewer;
        self.palette.set_feedback(
            CommandFeedbackKind::Notice,
            i18n("agent-session-opening-recent").to_string(),
            cx,
        );

        if caps.session_resume {
            // The backend answers whether the request reached a session that
            // could take it, because a conversation rooted somewhere else is
            // one this tab cannot adopt.
            if !self
                .runtime
                .backend
                .as_mut()
                .is_some_and(|session| session.resume_thread(&id))
            {
                self.history_ui.mode = RecentSessionsMode::Open;
                self.runtime.status = previous_status;
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-session-codex-recent-not-ready").to_string(),
                    cx,
                );
            }
        } else {
            // Without in-session resume the conversation is a file this side
            // reads, so the pane respawns against its id and replays what it
            // read into the fresh session.
            let kind = self.kind;
            let cwd = self.cwd();
            let replay_id = id.clone();
            let selected = index;

            cx.spawn(async move |this, cx| {
                let replay = cx
                    .background_executor()
                    .spawn(async move { sessions::load_replay(cwd.as_deref(), &replay_id) })
                    .await;

                let _ = this.update(cx, |this, cx| {
                    if this.history_ui.mode != RecentSessionsMode::Loading
                        || this.history_ui.selected != selected
                    {
                        return;
                    }

                    this.start_session_with_options(
                        Some(RecoveryIdentity::new(kind, id)),
                        false,
                        move |this, started, _| {
                            if started {
                                this.history_ui.pending_resume_replay = Some(replay);
                            } else {
                                this.history_ui.mode = RecentSessionsMode::Open;
                            }
                        },
                        cx,
                    );
                });
            })
            .detach();
        }
    }
}
