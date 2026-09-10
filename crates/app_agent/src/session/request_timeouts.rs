use std::time::{Duration, Instant};

use gpui::Context;

use crate::AgentPane;
use crate::session::Status;

impl AgentPane {
    pub(super) fn start_request_watchdog(&self, epoch: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |pane, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep_running = pane
                    .update(cx, |pane, cx| {
                        if pane.runtime.epoch != epoch
                            || matches!(pane.runtime.status, Status::Exited)
                        {
                            return false;
                        }
                        let Some(backend) = pane.runtime.backend.as_mut() else {
                            return false;
                        };
                        let events = backend.poll_timeouts(Instant::now());
                        let changed = !events.is_empty();
                        for event in events {
                            pane.apply_event(event, cx);
                        }
                        if changed {
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }
}
