use gpui::Context;
use nmt_agent::chat::Event;
use nmt_agent::session::restore::{ReplayLoaded, ReplayRead};

use crate::agent_tab::execution::AgentSession;

impl AgentSession {
    pub(in crate::agent_tab) fn read_resume(
        &mut self,
        request: ReplayRead,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let (request, replay) = cx
                .background_executor()
                .spawn(async move {
                    let replay = request.load();

                    (request, replay)
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.is_closed() {
                    return;
                }

                let result = {
                    let mut guard = this.controller.borrow_mut();
                    let state = &mut *guard;

                    state.restore.loaded(
                        &mut state.runtime,
                        request,
                        this.active_workspace.primary(),
                        replay,
                    )
                };

                match result {
                    ReplayLoaded::Stale => {}
                    ReplayLoaded::Cancelled => cx.notify(),

                    ReplayLoaded::Failed(message) => {
                        let epoch = this.controller.borrow().runtime.epoch();

                        this.apply_event(
                            epoch,
                            Event::Error {
                                message,
                                fatal: false,
                            },
                            cx,
                        );
                    }

                    ReplayLoaded::Restart(identity) => {
                        this.start(Some(identity), false, |_, _| {}, cx)
                    }
                }
            });
        })
        .detach();
    }
}
