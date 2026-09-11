//! The pane's state, grouped by the concern that changes it.
//!
//! [`crate::AgentPane`] coordinates one conversation across a backend
//! process, per-turn bookkeeping, and child-agent activity. Each group below
//! holds the fields one of those concerns mutates together, so a reader can
//! tell from the type which fields move as a unit and which merely live on
//! the same pane.

use std::time::{Duration, Instant};

use gpui::Context;

use crate::AgentPane;
use crate::session::turn::response_age_tick;

/// Timing shown for the current turn and the most recently settled response.
pub(crate) struct TurnPresentation {
    /// When the running turn was handed to the backend, kept until its first
    /// visible output answers it. Measured from submission rather than from
    /// the backend's turn-started event, so the reading covers the whole wait
    /// the user sat through, CLI and RPC latency included.
    pub(crate) submitted_at: Option<Instant>,
    /// How long the last turn took to produce anything visible. Survives the
    /// turn so the composer keeps reporting it while the conversation is
    /// idle.
    pub(crate) first_output_latency: Option<Duration>,
    /// When the agent last finished answering, for the composer's idle reading
    /// of how long the conversation has been waiting on the user. `None` until
    /// the first turn settles.
    pub(crate) last_response_at: Option<Instant>,
}

impl TurnPresentation {
    pub(crate) fn last_response_at(&self) -> Option<Instant> {
        self.last_response_at
    }

    pub(crate) fn forget_last_response(&mut self) {
        self.last_response_at = None;
    }

    /// Stamp the moment the agent stopped answering and keep the composer's
    /// reading of it current.
    ///
    /// The label's resolution decides the cadence: a reading in seconds has to
    /// be redrawn every second, one in minutes only every minute. A pane whose
    /// last answer was an hour ago would otherwise hold the frame pump awake
    /// for a label that has not changed.
    pub(crate) fn note_response_settled(&mut self, at: Instant, cx: &mut Context<AgentPane>) {
        let restart = self.last_response_at.is_none();

        self.last_response_at = Some(at);

        if !restart {
            return;
        }

        cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |this, cx| {
                    cx.notify();

                    this.turn
                        .last_response_at()
                        .and_then(|at| response_age_tick(at.elapsed()))
                }) else {
                    break;
                };

                let Some(interval) = interval else {
                    break;
                };

                cx.background_executor().timer(interval).await;
            }
        })
        .detach();
    }

    /// Note that the running turn has produced something visible.
    ///
    /// Only the first output of a turn answers "how long until it said
    /// something", so taking the stamp both records the reading and closes the
    /// measurement for the rest of the turn.
    pub(crate) fn note_visible_output(&mut self) {
        if let Some(submitted_at) = self.submitted_at.take() {
            self.first_output_latency = Some(submitted_at.elapsed());
        }
    }
}
