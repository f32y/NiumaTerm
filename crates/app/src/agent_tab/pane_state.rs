//! Repaint scheduling for the response-age label.

use gpui::{Context, Task};

use crate::agent_tab::AgentPane;
use crate::agent_tab::session::turn::response_age_tick;

#[derive(Default)]
pub(in crate::agent_tab) struct TurnPresentation {
    timer: Option<Task<()>>,
}

impl TurnPresentation {
    pub(in crate::agent_tab) fn refresh_timer(&mut self, cx: &mut Context<AgentPane>) {
        if self.timer.is_some() {
            return;
        }

        self.timer = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |this, cx| {
                    cx.notify();

                    this.session
                        .borrow()
                        .conversation
                        .borrow()
                        .last_response_at
                        .and_then(|at| response_age_tick(at.elapsed()))
                }) else {
                    return;
                };

                let Some(interval) = interval else {
                    let _ = this.update(cx, |this, _| this.turn.timer = None);

                    return;
                };

                cx.background_executor().timer(interval).await;
            }
        }));

        cx.notify();
    }
}
