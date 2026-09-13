use gpui::Context;
use nmt_agent::session::branch::{BranchUpdate, CheckpointRead};
use nmt_agent::session::controller::SessionEffect;

use crate::agent_tab::execution::AgentSession;

impl AgentSession {
    pub(crate) fn read_checkpoints(&mut self, request: CheckpointRead, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (request, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = request.load();

                    (request, result)
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.is_closed() {
                    return;
                }

                let update = {
                    let mut state = this.controller.borrow_mut();
                    let epoch = state.runtime.epoch();

                    state.branch.checkpoints_loaded(epoch, request, result)
                };

                this.on_branch_update(update, cx);
            });
        })
        .detach();
    }

    pub(crate) fn on_branch_update(&mut self, update: BranchUpdate, cx: &mut Context<Self>) {
        if self.is_closed() {
            return;
        }

        match update {
            BranchUpdate::CreateFork(request) => {
                cx.spawn(async move |this, cx| {
                    let (request, result) = cx
                        .background_executor()
                        .spawn(async move {
                            let result = request.run();

                            (request, result)
                        })
                        .await;

                    let _ = this.update(cx, |this, cx| {
                        if this.is_closed() {
                            return;
                        }

                        let update = {
                            let mut state = this.controller.borrow_mut();
                            let epoch = state.runtime.epoch();

                            state.branch.fork_created(epoch, request, result)
                        };

                        this.on_branch_update(update, cx);
                    });
                })
                .detach();
            }

            BranchUpdate::StartSession(identity) => {
                self.controller.borrow_mut().commands.clear();
                self.start(identity, true, |_, _| {}, cx);
            }

            BranchUpdate::Branching => {
                let mut state = self.controller.borrow_mut();

                state.restore.cancel();
                state.controls.seed_thread_defaults = false;
                state.controls.seed_approval_reviewer = false;
                drop(state);
                self.publish(SessionEffect::Branch(BranchUpdate::Branching), cx);
            }

            update => self.publish(SessionEffect::Branch(update), cx),
        }
    }
}
