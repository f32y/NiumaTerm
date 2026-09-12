use std::time::{Duration, Instant};

use gpui::Context;
use nmt_agent::AgentEventKind;
use nmt_agent::chat::QuestionMode;
use nmt_agent::session::controller::QuestionSubmission;
use nmt_agent::session::input::QuestionAction;

use crate::agent_tab::execution::AgentSession;

impl AgentSession {
    pub(crate) fn expire_optional_question(&mut self, index: usize, cx: &mut Context<Self>) {
        let (optional, key) = {
            let state = self.controller.borrow();
            let prompt = &state.input.batches()[index];

            (prompt.mode() == QuestionMode::Optional, prompt.key())
        };

        if !optional {
            return;
        }

        let epoch = self.controller.borrow().runtime.epoch();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let keep_running = this.update(cx, |this, cx| {
                    if this.is_closed()
                        || !this.controller.borrow().runtime.is_current(epoch)
                        || this
                            .controller
                            .borrow()
                            .runtime
                            .update_suspension()
                            .is_some()
                    {
                        return false;
                    }

                    let state = this.controller.borrow();

                    let Some(prompt) = state
                        .input
                        .batches()
                        .iter()
                        .find(|prompt| prompt.key() == key)
                    else {
                        return false;
                    };

                    let Some(remaining) = prompt.auto_resolve_remaining(Instant::now()) else {
                        return false;
                    };

                    drop(state);

                    if remaining.is_zero() {
                        let outcome = this.controller.borrow_mut().submit_question(
                            key,
                            QuestionAction::Timeout,
                            Instant::now(),
                        );

                        if matches!(
                            outcome,
                            QuestionSubmission::Settled {
                                waiting_finished: true
                            }
                        ) {
                            this.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                        }

                        cx.notify();

                        return false;
                    }

                    cx.notify();

                    true
                });

                if !keep_running.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }
}
