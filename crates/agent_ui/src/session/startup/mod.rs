//! Schedule provider startup and deliver bounded event batches to the view.

use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::Context;
use nmt_agent::chat::Event::{self, self as SessionEvent};
use nmt_agent::chat::Item as SessionItem;
use nmt_agent::message_memory::OUTPUT_FAILURE_METHOD;
use nmt_agent::session::lifecycle::StartOutcome;
use nmt_agent::session::restore::SettingsSeed;
use nmt_config::profile::AgentProfileKind;
use nmt_i18n::i18n;
use parking_lot::Mutex;
use serde_json::Value;
use tracing::info;

use crate::capabilities::AgentCapabilities as _;
use crate::composer::CommandFeedbackKind;
use crate::profile::{AgentKind, agent_launch};
use crate::session::{Backend, RecoveryIdentity, Status};
use crate::settings::AgentSettings;
use crate::thread_controls::{launch_effort, launch_model, stored_thread_settings};
use crate::{AgentPane, AgentPaneEvent, RecentSessionsMode};

impl AgentPane {
    /// Whether the cover is on screen right now.
    ///
    /// Read from the start's own state rather than latched on and off around
    /// it. A harness's process exists well before the harness answers: Codex
    /// spawns in a moment and then reads its configuration and catalogs, and
    /// the pane stays in `Status::Starting` until the thread-ready message
    /// arrives. Tying the cover to the spawn instead put it on screen after
    /// the process was already up and left it there once the harness was
    /// ready.
    pub(crate) fn shows_start_overlay(&self) -> bool {
        self.session.runtime.status() == Status::Starting
    }

    pub(crate) fn start_session(&mut self, resume: Option<String>, cx: &mut Context<Self>) {
        self.start_session_with_options(
            resume.map(|id| RecoveryIdentity::new(AgentKind::Claude, id)),
            false,
            |_, _, _| {},
            cx,
        )
    }

    /// `on_result` runs once the backend either came up or failed to, carrying
    /// whether it did. The spawn no longer answers that on the calling stack,
    /// so a caller that reports the outcome does it from there.
    pub(crate) fn start_session_with_options(
        &mut self,
        recovery: Option<RecoveryIdentity>,
        preserve_thread_settings: bool,
        on_result: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        // The pane's profile is a snapshot from when the tab opened; profile
        // edits in settings don't reach into live panes. Re-resolving by
        // name at every (re)start picks them up, so a new conversation
        // launches with the profile as currently configured. A renamed or
        // deleted profile keeps the snapshot so the tab still works.
        if let Some(fresh) = cx
            .global::<AgentSettings>()
            .profiles
            .iter()
            .find(|p| p.kind == self.profile.kind && p.name == self.profile.name)
        {
            self.profile = fresh.clone();
        }

        // The profile model is known before either CLI completes its
        // handshake, so the picker need not flash the backend default while a
        // custom endpoint is starting.
        if !preserve_thread_settings && let Some(model) = launch_model(self.kind, &self.profile) {
            self.session.controls.settings.model = Some(model);
        }

        // A pinned effort reaches the backend through the launch, so the
        // picker shows it from the first frame rather than the level the
        // agent would otherwise have used.
        if !preserve_thread_settings && let Some(effort) = launch_effort(&self.profile) {
            self.session.controls.settings.effort = Some(effort);
        }

        let kind = self.kind;
        let name = kind.display();

        // The conversation about to start owns this snapshot for its whole
        // life; a later workspace edit reaches the one after it.
        self.active_workspace = self.workspace.clone();

        let workspace = self.active_workspace.clone();

        let caps = kind.caps();

        // A resume into a backend that replays its own thread controls keeps
        // them; anything else starts from the remembered picks. The reviewer is
        // seeded separately because a backend can replay the rest without it.
        let seed = if preserve_thread_settings {
            SettingsSeed::None
        } else if recovery.is_some() {
            SettingsSeed::resumed(kind)
        } else {
            SettingsSeed::Defaults
        };
        self.seed_restored_settings(seed);
        self.session.controls.restore_on_ready =
            preserve_thread_settings.then(|| self.session.controls.settings.clone());

        // Replacing a conversation must clear any running or unread state
        // associated with the previous backend before the new epoch can emit.
        cx.emit(AgentPaneEvent::Interrupted);

        // The previous attempt's reason describes a backend nobody is waiting
        // on any more, and this start is what the pane now reports.
        let start = self.session.starting(recovery.as_ref());
        let epoch = start.epoch;
        self.prompts.release_secret_editors(&self.session.input);
        if start.reset_branch {
            self.branch.clear();
        }

        self.history_ui.invalidate_filesystem_history();

        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;

        let (tx, rx) = channel();
        let mut batches = rx.ready_chunks(MAX_MESSAGES_PER_BATCH);
        let deliver = move |message| {
            tx.send(message);
        };
        let mut launch = agent_launch(&self.profile);

        // A backend that builds its system prompt from the model it resolves at
        // launch would otherwise describe a different model than the one
        // serving the turns, because the pick would only reach the CLI
        // afterwards. The pane already knows the pick here: the profile
        // assigned it above, or it is the one remembered for this profile. A
        // tab with neither leaves the flag off and starts on the CLI's
        // configured model.
        if caps.model_baked_into_launch {
            launch.model = self.session.controls.settings.model.clone().or_else(|| {
                stored_thread_settings(self.kind, &self.profile, cx)
                    .and_then(|stored| stored.model.clone())
            });
        }

        let codex_host_catalog = if kind == AgentKind::Codex {
            cx.global::<AgentSettings>()
                .profiles
                .iter()
                .filter(|profile| profile.kind == AgentProfileKind::Codex)
                .map(agent_launch)
                .collect()
        } else {
            Vec::new()
        };

        // Env names only: the values can carry API keys.
        info!(
            "agent session start: profile=\"{}\", executable=\"{}\", model={:?}, env=[{}]",
            self.profile.name,
            launch.executable,
            launch.model,
            launch
                .env
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Process creation blocks for hundreds of milliseconds on Windows
        // (cmd.exe, then the CLI's own launcher), which is long enough to drop
        // frames if it runs on the UI thread. The pane already models the gap
        // as `Status::Starting` with no backend installed, so the spawn moves
        // to a background thread and the result arrives in a later update.
        let spawned = cx.background_executor().spawn(async move {
            Backend::spawn(
                kind,
                &launch,
                &codex_host_catalog,
                &workspace,
                recovery,
                deliver,
            )
        });

        cx.spawn(async move |this, cx| {
            let spawned = spawned.await;

            let started = this
                .update(cx, |this, cx| {
                    // A superseded start reports nothing: the newer one owns
                    // the pane's state, and this caller's outcome no longer
                    // describes what the pane is doing.
                    match this.install_started_session(spawned, epoch, name, cx) {
                        Some(started) => {
                            on_result(this, started, cx);
                            cx.notify();
                            started
                        }
                        None => false,
                    }
                })
                .unwrap_or(false);

            if !started {
                return;
            }

            while let Some(messages) = batches.next().await {
                let mut messages = messages.into_iter();

                while messages.len() > 0 {
                    let updated = this.update(cx, |this, cx| {
                        let started = Instant::now();
                        let mut events = EventBatch::default();

                        for message in messages.by_ref() {
                            // A transition during this batch can replace the backend.
                            // Remaining messages still belong to the earlier session.
                            if !this.session.runtime.is_current(epoch) {
                                return false;
                            }

                            let mut message = match message {
                                Ok(message) => message,
                                Err(error) => {
                                    events
                                        .flush(|event| this.apply_session_event(epoch, event, cx));

                                    if this.session.runtime.is_current(epoch) {
                                        this.stop_for_output_failure(error, cx);
                                    }

                                    return false;
                                }
                            };

                            let Some(next_events) =
                                this.session.runtime.process(epoch, message.take())
                            else {
                                return false;
                            };

                            for event in next_events {
                                events.push(event, |event| {
                                    this.apply_session_event(epoch, event, cx)
                                });
                            }

                            if started.elapsed() >= MAX_UPDATE_TIME {
                                break;
                            }
                        }

                        events.flush(|event| this.apply_session_event(epoch, event, cx));

                        true
                    });

                    if !updated.unwrap_or(false) {
                        return;
                    }

                    // An already-ready stream need not yield at its next await.
                    // Give input and frame work a chance between bounded slices.
                    cx.background_executor()
                        .timer(Duration::from_millis(1))
                        .await;
                }
            }

            let _ = this.update(cx, |this, cx| {
                // A deliberately replaced session exits by design;
                // only the live session's death is worth a line.
                let Some(exit_events) = this.session.runtime.process_exit(epoch) else {
                    return;
                };

                for event in exit_events {
                    this.apply_session_event(epoch, event, cx);
                }

                if !this.session.runtime.is_current(epoch) {
                    return;
                }

                cx.emit(AgentPaneEvent::Interrupted);
                let failure = this.session.disconnected(
                    &i18n("agent-session-exited-before-restored").replace("{name}", name),
                );
                if let Some(failure) = failure.branch {
                    this.history_ui.mode = RecentSessionsMode::Open;
                    this.report_branch_failure(failure, cx);
                }
                if failure.resume_failed {
                    this.history_ui.mode = RecentSessionsMode::Open;
                }
                if failure.cancelled_commands {
                    this.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        i18n("agent-session-queued-cancelled-exited").replace("{name}", name),
                        cx,
                    );
                }

                this.prompts.release_secret_editors(&this.session.input);
                this.publish_queued_user_messages(cx);
                this.finish_working(cx);
                this.push_item(
                    SessionItem::Error {
                        text: i18n("agent-session-exited").replace("{name}", name),
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    /// Applying an event can start another conversation, including while a
    /// batch is being flushed. Every remaining event still belongs to the
    /// session that produced that batch.
    fn apply_session_event(&mut self, epoch: u64, event: SessionEvent, cx: &mut Context<Self>) {
        let effect = self.session.apply_event(epoch, event);
        self.present_session_effect(effect, cx);
    }

    /// Take ownership of a backend that finished spawning, reporting whether it
    /// came up. `None` means a newer start superseded this one while the
    /// process was coming up; that leaves a live CLI behind, so the orphan is
    /// shut down rather than dropped.
    pub(super) fn install_started_session(
        &mut self,
        spawned: Result<Backend, String>,
        epoch: u64,
        name: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        let spawned = spawned.map_err(|error| {
            i18n("agent-session-start-failed")
                .replace("{name}", name)
                .replace("{error}", &error)
        });

        match self.session.install(epoch, spawned) {
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
                cx.emit(AgentPaneEvent::Interrupted);

                let turn = self.session.delivery.turn();

                self.transcript.update(cx, |transcript, _| {
                    transcript.push_stamped(turn, SessionItem::Error { text });
                });

                Some(false)
            }
        }
    }
}

impl AgentPane {
    pub(super) fn stop_for_output_failure(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(mut backend) = self.session.runtime.retire() {
            for event in backend.process_exit() {
                self.apply_event(event, cx);
            }

            cx.background_executor()
                .spawn(async move {
                    let _ = backend.shutdown(Duration::from_millis(250), true);
                })
                .detach();
        }

        self.apply_event(
            Event::Error {
                message: error,
                fatal: true,
            },
            cx,
        );
        cx.notify();
    }
}

struct Message {
    value: Option<Value>,
}

impl Message {
    fn take(&mut self) -> Value {
        self.value.take().expect("queued message is consumed once")
    }
}

struct Sender {
    sender: Mutex<Option<mpsc::UnboundedSender<Result<Message, String>>>>,
}

fn channel() -> (Sender, mpsc::UnboundedReceiver<Result<Message, String>>) {
    let (sender, receiver) = mpsc::unbounded();

    (
        Sender {
            sender: Mutex::new(Some(sender)),
        },
        receiver,
    )
}

impl Sender {
    fn send(&self, value: Value) {
        // Serialize admission and terminal failure so no later producer can
        // publish a message after the stream has become incomplete.
        let mut sender = self.sender.lock();
        let Some(tx) = sender.as_ref() else { return };
        if value["method"] == OUTPUT_FAILURE_METHOD {
            let error = value["params"]["message"]
                .as_str()
                .unwrap_or("Agent output failed")
                .to_string();

            let _ = tx.unbounded_send(Err(error));

            sender.take();
            return;
        }

        let message = Message { value: Some(value) };

        if tx.unbounded_send(Ok(message)).is_err() {
            sender.take();
        }
    }
}

#[cfg(test)]
mod inbox_tests;

const MAX_MESSAGES_PER_BATCH: usize = 64;
const MAX_UPDATE_TIME: Duration = Duration::from_millis(2);

/// Only adjacent deltas can share an update. Every other event is applied
/// immediately, so approvals and turn transitions retain their side effects
/// before the next backend message is processed.
#[derive(Default)]
struct EventBatch {
    pending: Option<Event>,
}

impl EventBatch {
    fn push(&mut self, event: Event, mut apply: impl FnMut(Event)) {
        match (&mut self.pending, event) {
            (
                Some(Event::AgentMessageDelta { item_id, delta }),
                Event::AgentMessageDelta {
                    item_id: next_id,
                    delta: next,
                },
            )
            | (
                Some(Event::ReasoningSummaryDelta { item_id, delta }),
                Event::ReasoningSummaryDelta {
                    item_id: next_id,
                    delta: next,
                },
            )
            | (
                Some(Event::CommandOutputDelta { item_id, delta }),
                Event::CommandOutputDelta {
                    item_id: next_id,
                    delta: next,
                },
            ) if *item_id == next_id => delta.push_str(&next),
            (_, event) => {
                self.flush(&mut apply);

                match event {
                    Event::AgentMessageDelta { .. }
                    | Event::ReasoningSummaryDelta { .. }
                    | Event::CommandOutputDelta { .. } => self.pending = Some(event),
                    event => apply(event),
                }
            }
        }
    }

    fn flush(&mut self, mut apply: impl FnMut(Event)) {
        if let Some(event) = self.pending.take() {
            apply(event);
        }
    }
}

#[cfg(test)]
mod output_tests;
