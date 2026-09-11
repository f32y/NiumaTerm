use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui::Context;
use nmt_agent::chat::{Event, Item, ThreadSettings};
use nmt_agent::session::controller::{ReadyDefaults, SessionEffect};
use nmt_agent::session::lifecycle::StartOutcome;
use nmt_agent::session::restore::SettingsSeed;
use nmt_agent::session::{Backend, RecoveryIdentity};
use nmt_config::profile::AgentProfileKind;
use nmt_i18n::i18n;

use crate::AgentPaneEvent;
use crate::capabilities::AgentCapabilities as _;
use crate::execution::AgentSession;
use crate::execution::inbox::{EventBatch, MAX_MESSAGES_PER_BATCH, MAX_UPDATE_TIME, channel};
use crate::profile::{AgentKind, agent_launch};
use crate::settings::AgentSettings;
use crate::thread_controls::{launch_effort, launch_model, stored_thread_settings};

impl AgentSession {
    pub(crate) fn reset(&mut self, cx: &mut Context<Self>) {
        if self.is_closed() {
            return;
        }

        let retiring = {
            let mut state = self.controller.borrow_mut();
            let retiring = state.runtime.retire();

            state.clear_conversation();
            state.controls.settings = ThreadSettings::default();
            state.controls.models.clear();
            state.command_catalog = None;
            state.skill_catalog = None;
            state.commands.clear();

            retiring
        };

        cx.emit(AgentPaneEvent::TitleSuggested(String::new()));
        self.start(None, false, move |_, _| drop(retiring), cx);
    }

    pub(crate) fn start(
        &mut self,
        recovery: Option<RecoveryIdentity>,
        preserve_settings: bool,
        on_result: impl FnOnce(bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.is_closed() {
            return;
        }

        if let Some(profile) = cx
            .global::<AgentSettings>()
            .profiles
            .iter()
            .find(|profile| profile.kind == self.profile.kind && profile.name == self.profile.name)
        {
            self.profile = profile.clone();
        }

        self.active_workspace = self.workspace.clone();

        let kind = self.kind;
        let name = kind.display();
        let mut launch = agent_launch(&self.profile);

        let epoch = {
            let mut session = self.controller.borrow_mut();

            if !preserve_settings {
                if let Some(model) = launch_model(kind, &self.profile) {
                    session.controls.settings.model = Some(model);
                }

                if let Some(effort) = launch_effort(&self.profile) {
                    session.controls.settings.effort = Some(effort);
                }
            }

            let seed = if preserve_settings {
                SettingsSeed::None
            } else if recovery.is_some() {
                SettingsSeed::resumed(kind)
            } else {
                SettingsSeed::Defaults
            };

            session.controls.seed_thread_defaults = matches!(seed, SettingsSeed::Defaults);
            session.controls.seed_approval_reviewer = matches!(seed, SettingsSeed::Reviewer);

            session.controls.restore_on_ready =
                preserve_settings.then(|| session.controls.settings.clone());

            if kind.caps().model_baked_into_launch {
                launch.model = session.controls.settings.model.clone().or_else(|| {
                    stored_thread_settings(kind, &self.profile, cx)
                        .and_then(|stored| stored.model.clone())
                });
            }

            session.starting(recovery.as_ref()).epoch
        };

        cx.emit(AgentPaneEvent::Interrupted);
        self.prepare_defaults(cx);

        let workspace = self.active_workspace.clone();

        let catalog = if kind == AgentKind::Codex {
            cx.global::<AgentSettings>()
                .profiles
                .iter()
                .filter(|profile| profile.kind == AgentProfileKind::Codex)
                .map(agent_launch)
                .collect()
        } else {
            Vec::new()
        };

        let (sender, receiver) = channel();
        let mut batches = receiver.ready_chunks(MAX_MESSAGES_PER_BATCH);

        let spawned = cx.background_executor().spawn(async move {
            Backend::spawn(
                kind,
                &launch,
                &catalog,
                &workspace,
                recovery,
                move |message| sender.send(message),
            )
        });

        cx.spawn(async move |this, cx| {
            let spawned = spawned.await;

            let installed = this
                .update(cx, |this, cx| {
                    let installed = this.install(spawned, epoch, name, cx);

                    if let Some(started) = installed {
                        on_result(started, cx);
                    }

                    installed == Some(true)
                })
                .unwrap_or(false);

            if !installed {
                return;
            }

            while let Some(messages) = batches.next().await {
                let mut messages = messages.into_iter();

                while messages.len() > 0 {
                    let alive = this
                        .update(cx, |this, cx| {
                            let started = Instant::now();
                            let mut events = EventBatch::default();

                            for message in messages.by_ref() {
                                if this.is_closed()
                                    || !this.controller.borrow().runtime.is_current(epoch)
                                {
                                    return false;
                                }

                                let mut message = match message {
                                    Ok(message) => message,

                                    Err(error) => {
                                        events.flush(|event| this.apply_event(epoch, event, cx));

                                        if this.controller.borrow().runtime.is_current(epoch) {
                                            this.stop_for_output_failure(error, cx);
                                        }

                                        return false;
                                    }
                                };

                                let next = this
                                    .controller
                                    .borrow_mut()
                                    .runtime
                                    .process(epoch, message.take());

                                let Some(next) = next else {
                                    return false;
                                };

                                for event in next {
                                    events.push(event, |event| this.apply_event(epoch, event, cx));
                                }

                                if started.elapsed() >= MAX_UPDATE_TIME {
                                    break;
                                }
                            }

                            events.flush(|event| this.apply_event(epoch, event, cx));

                            true
                        })
                        .unwrap_or(false);

                    if !alive {
                        return;
                    }

                    cx.background_executor()
                        .timer(Duration::from_millis(1))
                        .await;
                }
            }

            let _ = this.update(cx, |this, cx| {
                if this.is_closed() {
                    return;
                }

                let events = this.controller.borrow_mut().runtime.process_exit(epoch);

                let Some(events) = events else {
                    return;
                };

                for event in events {
                    this.apply_event(epoch, event, cx);
                }

                if !this.controller.borrow().runtime.is_current(epoch) {
                    return;
                }

                this.apply_event(
                    epoch,
                    Event::Error {
                        message: i18n("agent-session-exited").replace("{name}", name),
                        fatal: true,
                    },
                    cx,
                );
            });
        })
        .detach();

        cx.notify();
    }

    pub(crate) fn prepare_defaults(&self, cx: &Context<Self>) {
        let mut session = self.controller.borrow_mut();
        let seed = session.controls.seed_thread_defaults;

        session.ready_defaults = ReadyDefaults {
            stored: (seed || session.controls.seed_approval_reviewer)
                .then(|| stored_thread_settings(self.kind, &self.profile, cx).cloned())
                .flatten(),
            model: seed
                .then(|| launch_model(self.kind, &self.profile))
                .flatten(),
            effort: seed.then(|| launch_effort(&self.profile)).flatten(),
        };
    }

    pub(crate) fn install(
        &mut self,
        spawned: Result<Backend, String>,
        epoch: u64,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        let spawned = spawned.map_err(|error| {
            i18n("agent-session-start-failed")
                .replace("{name}", name)
                .replace("{error}", &error)
        });

        let outcome = self.controller.borrow_mut().install(epoch, spawned);

        match outcome {
            StartOutcome::Installed => Some(true),

            StartOutcome::Superseded(orphan) => {
                if let Some(mut orphan) = orphan {
                    cx.background_executor()
                        .spawn(async move {
                            let _ = orphan.shutdown(Duration::from_secs(5), true);
                        })
                        .detach();
                }

                None
            }

            StartOutcome::Failed(text) => {
                let failure = self.controller.borrow_mut().failed(&text, true);

                if self
                    .controller
                    .borrow()
                    .runtime
                    .last_recovery_snapshot()
                    .is_some()
                {
                    self.controller
                        .borrow_mut()
                        .runtime
                        .recovery_failed(text.clone());
                }

                self.controller
                    .borrow_mut()
                    .push_item(Item::Error { text: text.clone() });

                self.publish(
                    SessionEffect::Error {
                        message: text,
                        fatal: true,
                        failure,
                    },
                    cx,
                );

                cx.emit(AgentPaneEvent::Interrupted);
                cx.notify();

                Some(false)
            }
        }
    }

    pub(crate) fn stop_for_output_failure(&mut self, error: String, cx: &mut Context<Self>) {
        let backend = self.controller.borrow_mut().runtime.retire();
        let epoch = self.controller.borrow().runtime.epoch();

        if let Some(mut backend) = backend {
            for event in backend.process_exit() {
                self.apply_event(epoch, event, cx);
            }

            cx.background_executor()
                .spawn(async move {
                    let _ = backend.shutdown(Duration::from_millis(250), true);
                })
                .detach();
        }

        self.apply_event(
            epoch,
            Event::Error {
                message: error,
                fatal: true,
            },
            cx,
        );
    }
}
