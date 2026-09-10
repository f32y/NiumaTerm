//! A turn from the moment work starts to the moment it settles, and the
//! prompts the agent raises inside one.
//!
//! The transcript reads how long the conversation has been waiting from when
//! the last answer settled, so the moments recorded here are what the idle
//! reading is built on.

use std::time::{Duration, Instant};

use chrono::Utc;
use gpui::{Context, Window};
use nmt_agent::AgentEventKind;
use nmt_agent::session::Backend;
use nmt_agent::session::lifecycle::InterruptOutcome;

use crate::composer::{CommandFeedbackKind, restored_input_after_interruption};
use crate::transcript::LAST_RESPONSE_LIMIT;
use crate::{AgentPane, AgentPaneEvent};

/// How long the composer's "last response" reading stays accurate, given how
/// old it already is. Matches the coarsest unit the label shows, so the pane
/// redraws exactly as often as the words change, and `None` once the label has
/// settled on "more than an hour" and will never change again.
/// How long ago a resumed conversation was answered, from the wall-clock stamp
/// the provider recorded for it.
///
/// The age is clamped to the span the label distinguishes: everything past it
/// reads "more than an hour ago" and passes every idle threshold a profile can
/// warn at, and the clamp keeps the caller's subtraction inside the monotonic
/// clock's range, which on Windows starts at boot and so cannot reach back to a
/// conversation from before the last restart. A stamp from ahead of this
/// machine's clock is idle time that has not happened.
pub(super) fn replayed_response_age(at_unix: i64, now_unix: i64) -> Duration {
    let seconds = u64::try_from(now_unix.saturating_sub(at_unix)).unwrap_or(0);
    Duration::from_secs(seconds).min(LAST_RESPONSE_LIMIT)
}

pub(crate) fn response_age_tick(age: Duration) -> Option<Duration> {
    const MINUTE: u64 = 60;

    match age.as_secs() {
        ..MINUTE => Some(Duration::from_secs(1)),
        seconds if seconds < LAST_RESPONSE_LIMIT.as_secs() => Some(Duration::from_secs(MINUTE)),
        _ => None,
    }
}

impl AgentPane {
    pub(crate) fn note_visible_output(&mut self) {
        self.delivery.visible_output();
        self.turn.note_visible_output();
    }

    /// Start the turn clock and drive the once-a-second repaint of the live
    /// progress row; the ticker stops itself once `finish_working` clears it.
    pub(crate) fn start_working(&mut self, cx: &mut Context<Self>) {
        self.turn.submitted_at = Some(Instant::now());
        self.transcript
            .update(cx, |transcript, cx| transcript.start_working(cx));
        cx.notify();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let ticking = this.update(cx, |this, cx| {
                    if this.transcript.read(cx).is_working() {
                        cx.notify();
                        true
                    } else {
                        false
                    }
                });

                if !ticking.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Settle the current turn's duration and exact output usage for its status
    /// row. These values are UI state rather than provider transcript content,
    /// so they stay outside the shared item stream.
    pub(super) fn finish_working(&mut self, cx: &mut Context<Self>) {
        let turn = self.delivery.turn();

        self.transcript
            .update(cx, |transcript, cx| transcript.settle_turn(turn, cx));
        self.turn.note_response_settled(Instant::now(), cx);
        cx.notify();
    }

    /// Carry a resumed conversation's own last answer into the reading, from
    /// the provider's wall-clock stamp for it.
    ///
    /// Without this the reading restarts at the resume, which reads as a warm
    /// conversation and skips the cold-prompt-cache warning in front of the
    /// first message — the one send where the cache is certainly gone.
    pub(super) fn note_replayed_response(&mut self, at_unix: i64, cx: &mut Context<Self>) {
        let age = replayed_response_age(at_unix, Utc::now().timestamp());
        let now = Instant::now();

        self.turn
            .note_response_settled(now.checked_sub(age).unwrap_or(now), cx);
    }

    pub(crate) fn interrupt_from_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let working = self.delivery.is_active();

        if let Some((turn, prompt)) = self.delivery.take_interrupted_prompt() {
            self.transcript
                .update(cx, |transcript, cx| transcript.discard_turn(turn, cx));

            let current = self.input.read(cx).text().to_string();
            let restored = restored_input_after_interruption(&prompt.text, &current);
            let cursor = restored.len();

            self.input.update(cx, |input, cx| {
                input.set_value(restored, window, cx);
                input.set_selected_range(cursor..cursor, cx);
            });
            self.palette.skill_binding = prompt.skill;
            self.attachments
                .restore_annotations(prompt.response_annotations);
            cx.notify();
        }

        let outcome = self
            .runtime
            .interrupt(working.then_some(self.delivery.turn()));

        self.present_interrupt_result(outcome, cx);
    }

    pub(super) fn interrupt(&mut self, cx: &mut Context<Self>) {
        let outcome = self.runtime.interrupt(None);
        self.present_interrupt_result(outcome, cx);
    }

    fn present_interrupt_result(&mut self, outcome: InterruptOutcome, cx: &mut Context<Self>) {
        match outcome {
            InterruptOutcome::Unavailable => {}
            InterruptOutcome::Rejected => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    "The interrupt request could not be queued.",
                    cx,
                );
            }
            InterruptOutcome::Accepted => {
                cx.emit(AgentPaneEvent::Interrupted);
                cx.notify();
            }
        }
    }

    pub(crate) fn respond_approval(&mut self, decision: &str, cx: &mut Context<Self>) {
        let accepted = self
            .runtime
            .backend_mut()
            .is_some_and(|session| session.respond_approval(decision));

        if accepted {
            if !matches!(self.runtime.backend(), Some(Backend::DeepSeek(_))) {
                self.prompts.dismiss_approval();
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
            }
        } else {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                "The approval response could not be queued.",
                cx,
            );
        }

        cx.notify();
    }
}
