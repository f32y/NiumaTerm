//! A turn from the moment work starts to the moment it settles, and the
//! prompts the agent raises inside one.
//!
//! The transcript reads how long the conversation has been waiting from when
//! the last answer settled, so the moments recorded here are what the idle
//! reading is built on.

use std::time::{Duration, Instant};

use chrono::Utc;
use gpui::{Context, Window};
use nmt_agent_utils::AgentEventKind;

use crate::composer::{PaletteControl, restored_input_after_interruption};
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

fn response_age_tick(age: Duration) -> Option<Duration> {
    const MINUTE: u64 = 60;

    match age.as_secs() {
        ..MINUTE => Some(Duration::from_secs(1)),
        seconds if seconds < LAST_RESPONSE_LIMIT.as_secs() => Some(Duration::from_secs(MINUTE)),
        _ => None,
    }
}

impl AgentPane {
    /// Start the turn clock and drive the once-a-second repaint of the live
    /// progress row; the ticker stops itself once `finish_working` clears it.
    pub(super) fn start_working(&mut self, cx: &mut Context<Self>) {
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
        let turn = self.turn.seq;
        self.transcript
            .update(cx, |transcript, cx| transcript.settle_turn(turn, cx));
        self.note_response_settled(Instant::now(), cx);
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
        self.note_response_settled(now.checked_sub(age).unwrap_or(now), cx);
    }

    /// Stamp the moment the agent stopped answering and keep the composer's
    /// reading of it current.
    ///
    /// The label's resolution decides the cadence: a reading in seconds has to
    /// be redrawn every second, one in minutes only every minute. A pane whose
    /// last answer was an hour ago would otherwise hold the frame pump awake
    /// for a label that has not changed.
    fn note_response_settled(&mut self, at: Instant, cx: &mut Context<Self>) {
        let restart = self.last_response_at.is_none();
        self.last_response_at = Some(at);

        if !restart {
            return;
        }

        cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |this, cx| {
                    cx.notify();
                    this.last_response_at
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

    pub(super) fn note_visible_agent_output(&mut self) {
        // Only the first output of a turn answers "how long until it said
        // something", so taking the stamp both records the reading and closes
        // the measurement for the rest of the turn.
        if let Some(submitted_at) = self.turn.submitted_at.take() {
            self.turn.first_output_latency = Some(submitted_at.elapsed());
        }

        if self
            .turn
            .unanswered_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.turn == self.turn.seq)
        {
            self.turn.unanswered_prompt = None;
        }
    }

    pub(crate) fn interrupt_from_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let working = self.transcript.read(cx).is_working();
        if working {
            self.turn.pending_interrupt = Some(self.turn.seq);
        }
        if let Some(prompt) = self
            .turn
            .unanswered_prompt
            .take()
            .filter(|prompt| prompt.turn == self.turn.seq && working)
        {
            let turn = prompt.turn;
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
            let mut response_annotations = prompt.response_annotations;
            response_annotations.append(&mut self.response_annotations);
            self.response_annotations = response_annotations;
            cx.notify();
        }

        self.interrupt(cx);
    }

    pub(super) fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.runtime.backend.as_mut() {
            session.interrupt();
            cx.emit(AgentPaneEvent::Interrupted);
            cx.notify();
        }
    }

    pub(crate) fn respond_approval(&mut self, decision: &str, cx: &mut Context<Self>) {
        // The card is dismissed immediately for a snappy UI; the session's
        // `ApprovalResolved` confirmation is then an idempotent status refresh.
        self.pending_approval = None;
        self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

        if let Some(session) = self.runtime.backend.as_mut() {
            session.respond_approval(decision);
        }
        cx.notify();
    }

    /// Record a pick without answering yet; the card stays open until the user
    /// submits, so multi-select questions can accumulate choices.
    pub(crate) fn toggle_question_option(
        &mut self,
        question: usize,
        option: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(prompt) = self.pending_questions.as_mut() {
            prompt.toggle(question, option);
            // Clicking also moves the highlight, so a switch to the keyboard
            // continues from the option the user just touched rather than from
            // wherever the arrows were left.
            prompt.focus = (question, option);
            cx.notify();
        }
    }

    /// Drive the question card from the keyboard. Returns whether the card
    /// consumed the key, so the caller can fall through to the surfaces that
    /// share these keys when no card is up.
    ///
    /// Enter answers the highlighted option rather than submitting the card:
    /// with several questions, or a multi-select one, the user is rarely done
    /// after one press, and a key that sometimes submits and sometimes selects
    /// cannot be predicted from what is on screen.
    pub(crate) fn handle_question_control(
        &mut self,
        control: PaletteControl,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(prompt) = self.pending_questions.as_mut() else {
            return false;
        };

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                if prompt.move_focus(matches!(control, PaletteControl::Next)) {
                    cx.stop_propagation();
                    cx.notify();
                    return true;
                }
                false
            }
            PaletteControl::Activate => {
                let (question, option) = prompt.focus;
                cx.stop_propagation();
                self.toggle_question_option(question, option, cx);
                true
            }
            // Completion belongs to the composer, and dismissing the card would
            // answer the question by refusing it, which needs the visible
            // control rather than a keystroke.
            PaletteControl::Complete | PaletteControl::Dismiss => false,
        }
    }

    /// Submit the current picks, or decline when `submit` is false. The card is
    /// dismissed immediately; the session's `QuestionsResolved` confirmation is
    /// then an idempotent status refresh, as with approvals.
    pub(crate) fn respond_questions(&mut self, submit: bool, cx: &mut Context<Self>) {
        let Some(prompt) = self.pending_questions.take() else {
            return;
        };

        let answers = (submit && prompt.is_complete()).then(|| prompt.answers());

        self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

        if let Some(session) = self.runtime.backend.as_mut() {
            session.respond_questions(answers);
        }
        cx.notify();
    }
}
