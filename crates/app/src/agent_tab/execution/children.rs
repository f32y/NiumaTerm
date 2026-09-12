use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::Context;
use nmt_agent::background_task::BackgroundTaskKey;
use nmt_agent::claude_code::sessions;

use crate::agent_tab::execution::AgentSession;

impl AgentSession {
    /// Rebuild Claude child agents from the session's persisted history. The
    /// read runs on a background thread and its failure never blocks the
    /// parent transcript or composer.
    pub(crate) fn restore_background_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self
            .controller
            .borrow()
            .runtime
            .backend()
            .and_then(|session| session.session_id())
            .map(str::to_owned)
        else {
            return;
        };

        if !self
            .controller
            .borrow_mut()
            .children
            .claim_restore(&session_id)
        {
            return;
        }

        let starting_sequence = {
            let mut state = self.controller.borrow_mut();

            let Some(session) = state.runtime.backend_mut() else {
                return;
            };

            session.begin_task_restoration()
        };

        let cwd = self.active_workspace.primary().map(str::to_owned);
        let epoch = self.controller.borrow().runtime.epoch();

        cx.spawn(async move |this, cx| {
            let restored = cx
                .background_executor()
                .spawn(async move { sessions::load_task_history(cwd.as_deref(), &session_id) })
                .await;

            let _ = this.update(cx, |this, cx| {
                if !this.controller.borrow().runtime.is_current(epoch) {
                    return;
                }

                let events = {
                    let mut state = this.controller.borrow_mut();

                    let Some(session) = state.runtime.backend_mut() else {
                        return;
                    };

                    session.finish_task_restoration(restored, starting_sequence)
                };

                for event in events {
                    this.apply_event(epoch, event, cx);
                }
            });
        })
        .detach();
    }
}

/// Refresh interest ends with the reader and never retains the backend.
pub struct ChildReader {
    key: (u64, BackgroundTaskKey),
    readers: Rc<RefCell<HashMap<(u64, BackgroundTaskKey), usize>>>,
}

impl Drop for ChildReader {
    fn drop(&mut self) {
        let mut readers = self.readers.borrow_mut();

        if let Some(count) = readers.get_mut(&self.key) {
            *count -= 1;

            if *count == 0 {
                readers.remove(&self.key);
            }
        }
    }
}

impl AgentSession {
    pub(crate) fn watch_child(
        &mut self,
        key: &BackgroundTaskKey,
        cx: &mut Context<Self>,
    ) -> Option<ChildReader> {
        if self.is_closed() || self.controller.borrow().background_tasks().is_none() {
            return None;
        }

        let reader_key = (self.controller.borrow().runtime.epoch(), key.clone());

        let first = {
            let mut readers = self.child_readers.borrow_mut();
            let count = readers.entry(reader_key.clone()).or_default();

            *count += 1;

            *count == 1
        };

        if first {
            self.load_child(key, cx);
        }

        if self.child_refresh.is_none() {
            self.child_refresh = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;

                    let alive = this.update(cx, |this, cx| {
                        if this.is_closed() || this.child_readers.borrow().is_empty() {
                            return false;
                        }

                        let keys: Vec<_> = this.child_readers.borrow().keys().cloned().collect();

                        for (epoch, key) in keys {
                            if !this.controller.borrow().runtime.is_current(epoch) {
                                continue;
                            }

                            let active = this.controller.borrow().background_tasks().is_some_and(
                                |snapshot| {
                                    snapshot
                                        .tasks
                                        .iter()
                                        .any(|task| task.key == key && task.state.is_active())
                                },
                            );

                            if active {
                                this.load_child(&key, cx);
                            }
                        }

                        true
                    });

                    if !alive.unwrap_or(false) {
                        break;
                    }
                }

                let _ = this.update(cx, |this, _| this.child_refresh = None);
            }));
        }

        Some(ChildReader {
            key: reader_key,
            readers: self.child_readers.clone(),
        })
    }

    fn load_child(&mut self, key: &BackgroundTaskKey, cx: &mut Context<Self>) {
        let (epoch, events) = {
            let mut state = self.controller.borrow_mut();
            let epoch = state.runtime.epoch();

            let Some(backend) = state.runtime.backend_mut() else {
                return;
            };

            (
                epoch,
                backend.load_background_task_transcript(key, self.active_workspace.primary()),
            )
        };

        for event in events {
            self.apply_event(epoch, event, cx);
        }
    }
}
