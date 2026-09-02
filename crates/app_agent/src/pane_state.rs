//! The pane's state, grouped by the concern that changes it.
//!
//! [`crate::AgentPane`] coordinates one conversation across a backend
//! process, per-turn bookkeeping, and child-agent activity. Each group below
//! holds the fields one of those concerns mutates together, so a reader can
//! tell from the type which fields move as a unit and which merely live on
//! the same pane.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use gpui::Context;
use nmt_agent_utils::background_task::{
    BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskTranscript,
};
use nmt_agent_utils::chat::QueuedPrompt;

use crate::session::turn::response_age_tick;
use crate::session::{Backend, RecoverySnapshot, Status, UpdateSuspension};
use crate::{AgentPane, UnansweredPrompt};

/// The backend process and its lifecycle: everything a (re)spawn replaces.
pub(crate) struct SessionRuntime {
    pub(crate) backend: Option<Backend>,
    /// Bumped on every (re)spawn; the message pump and EOF handler of an
    /// older session compare against it and stand down, so deliberately
    /// replacing the session (resume) doesn't route stale messages into the
    /// new one or report a bogus exit.
    pub(crate) epoch: u64,
    pub(crate) status: Status,
    /// Why the backend never came up, while that is still the pane's whole
    /// state. Held rather than derived from [`Status::Exited`], which a
    /// conversation that ran and then ended also reaches.
    pub(crate) start_failure: Option<String>,
    /// Whether the start has taken long enough to be worth covering the tab
    /// for. Set by a timer rather than compared against a clock at render
    /// time, because a pane waiting on its backend repaints for nothing else.
    pub(crate) start_overlay_due: bool,
    /// Process replacement for a provider update is pane state rather than a
    /// terminal exit. Keeping it separate retains transcript and composer
    /// contents while preventing input from reaching a missing backend.
    pub(crate) update_suspension: Option<UpdateSuspension>,
    pub(crate) last_recovery_snapshot: Option<RecoverySnapshot>,
}

/// One turn's bookkeeping, from submission to settled output.
pub(crate) struct TurnState {
    /// Monotonic turn counter; entries are tagged with the turn they arrived
    /// in so a settled turn can fold as one unit.
    pub(crate) seq: u64,
    /// When the running turn was handed to the backend, kept until its first
    /// visible output answers it. Measured from submission rather than from
    /// the backend's turn-started event, so the reading covers the whole wait
    /// the user sat through, CLI and RPC latency included.
    pub(crate) submitted_at: Option<Instant>,
    /// How long the last turn took to produce anything visible. Survives the
    /// turn so the composer keeps reporting it while the conversation is
    /// idle.
    pub(crate) first_output_latency: Option<Duration>,
    /// The active prompt remains recoverable until provider activity becomes
    /// visible, allowing an immediate stop to return it to the composer.
    pub(crate) unanswered_prompt: Option<UnansweredPrompt>,
    /// Turn the user asked to stop. Interruption is a completion state of a
    /// turn, so the "Interrupted" transcript row is drawn only when that turn
    /// actually ends; a backend that keeps streaming past the stop request
    /// keeps its truthful working row until then.
    pub(crate) pending_interrupt: Option<u64>,
    /// Mid-turn inputs stay near the composer until provider activity
    /// confirms they have joined the running response. A backend that owns
    /// its own pending queue republishes this whole list, which is what gives
    /// the rows the identities a removal needs.
    pub(crate) queued_user_messages: VecDeque<QueuedPrompt>,
    /// The prompt this side already put in the transcript because the backend
    /// admitted it as a new turn. A backend that publishes its pending inbox
    /// keeps listing that prompt until the turn claims it, and every list it
    /// appears in would show the message a second time beside the row that is
    /// already there.
    pub(crate) published_prompt: Option<String>,
    /// When the agent last finished answering, for the composer's idle reading
    /// of how long the conversation has been waiting on the user. `None` until
    /// the first turn settles.
    pub(crate) last_response_at: Option<Instant>,
}

impl TurnState {
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

        if self
            .unanswered_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.turn == self.seq)
        {
            self.unanswered_prompt = None;
        }
    }
}

/// Child-agent activity the provider adapter reports for this conversation.
pub(crate) struct ChildAgents {
    /// Latest child-agent snapshot published by the provider adapter. The
    /// adapter owns child lifecycle; the pane keeps only this replacement
    /// copy so the right-side view never maintains a second mutable registry.
    pub(crate) background_tasks: Option<BackgroundTaskSnapshot>,
    /// Each child's own conversation, accumulated here rather than in the
    /// adapter so live activity is retained once and the retention bound
    /// applies to what is actually shown.
    pub(crate) transcripts: HashMap<BackgroundTaskKey, BackgroundTaskTranscript>,
    /// Claude session id whose child agents were already restored from
    /// history. Ready fires again during first-turn initialization, so the
    /// read happens once per conversation rather than once per confirmation.
    pub(crate) restored_session: Option<String>,
}
