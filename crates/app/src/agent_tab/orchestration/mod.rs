//! The app side of one orchestration run: the slot sessions that run its
//! nodes, and the queue that saves every change to the run.
//!
//! Saving writes files, which can block, so each change runs on the
//! background executor while it holds the run's store, one at a time in the
//! order queued. The UI reads the last saved copy of the run.

pub use crate::agent_tab::orchestration::view::OrchestrationPane;

mod canvas;
mod editor;
mod properties;
mod slot;
mod turn;
mod view;

#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, VecDeque};
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::channel::oneshot;
use gpui::{App, AppContext as _, BackgroundExecutor, Context, Entity, EventEmitter, Task};
use nmt_agent::AgentWorkspace;
use nmt_agent::agent_spec::ProfileReference;
use nmt_agent::chat::{Item, SendOutcome, ThreadSettings};
use nmt_agent::orchestration::compose::compose;
use nmt_agent::orchestration::graph::Graph;
use nmt_agent::orchestration::run::{NodeState, RunId, RunRecord, RunState};
use nmt_agent::orchestration::schedule::schedule;
use nmt_agent::orchestration::store::{RunStore, RunStoreError};
use nmt_agent::session::AgentKind;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::transcript::TranscriptEntry;
use nmt_agent::transcript::conversation::EntryMetadata;
use nmt_config::profile::AgentProfile;

use crate::agent_tab::execution::{AgentSession, ExecutionSignal, SessionOwner, TranscriptChanged};
use crate::agent_tab::orchestration::slot::{LiveNode, SlotHost, SlotObservation};
use crate::agent_tab::orchestration::turn::{saved_entries, turn_entries, turn_items};
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::thread_controls::launch_pins;

type Operation = Box<dyn FnOnce(&mut OrchestrationRuntime, &mut Context<OrchestrationRuntime>)>;

/// A node to fail: the node, the session epoch it was sent under (`None`
/// when it never was), and why.
type Failure = (usize, Option<u64>, String);

/// A slot whose profile is not saved, which keeps a run from starting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingProfile {
    pub slot: String,
    pub profile: ProfileReference,
}

pub struct OrchestrationRuntime {
    store: Option<RunStore>,
    run: Option<RunRecord>,
    graph: Option<Graph>,
    pending: VecDeque<Operation>,
    executor: BackgroundExecutor,
    slots: Vec<Option<SlotHost>>,
    error: Option<String>,
    scheduled: bool,
    refresh_again: bool,
    loading: bool,
    load_failed: bool,
    closed: bool,
}

impl EventEmitter<TranscriptChanged> for OrchestrationRuntime {}

impl OrchestrationRuntime {
    /// Start a new run of `graph`. The caller has checked that every slot's
    /// profile exists, with [`missing_profile`].
    pub fn start(
        directory: &Path,
        definition_name: String,
        graph: Graph,
        input: String,
        workspace: AgentWorkspace,
        cx: &mut App,
    ) -> Entity<Self> {
        let record = RunRecord::new(definition_name, &graph, input, workspace, now());
        let target = directory.to_owned();

        Self::load(Some(graph), move || RunStore::create(&target, record), cx)
    }

    /// Reopen a saved run. A run saved as going was cut off by the app
    /// closing; it is marked interrupted and sends nothing until resumed.
    pub fn open(directory: &Path, id: RunId, cx: &mut App) -> Entity<Self> {
        let target = directory.to_owned();

        Self::load(
            None,
            move || {
                let mut store = RunStore::open(&target, id)?;

                store.update(RunRecord::interrupt)?;

                Ok(store)
            },
            cx,
        )
    }

    fn load(
        graph: Option<Graph>,
        load: impl FnOnce() -> Result<RunStore, RunStoreError> + Send + 'static,
        cx: &mut App,
    ) -> Entity<Self> {
        let task = cx.background_executor().spawn(async move { load() });

        let entity = cx.new(|cx| Self {
            store: None,
            run: None,
            graph,
            pending: VecDeque::new(),
            executor: cx.background_executor().clone(),
            slots: Vec::new(),
            error: None,
            scheduled: false,
            refresh_again: false,
            loading: true,
            load_failed: false,
            closed: false,
        });

        entity.update(cx, |_, cx| {
            cx.on_app_quit(|this, cx| {
                this.close(cx);

                async {}
            })
            .detach();
        });

        let weak = entity.downgrade();

        cx.spawn(async move |cx| {
            let loaded = task.await;

            let _ = weak.update(cx, |this, cx| {
                this.loading = false;

                match loaded {
                    Ok(store) => {
                        this.install(store);
                        this.schedule(cx);
                        this.start_next(cx);
                    }
                    Err(error) => {
                        this.load_failed = true;
                        this.error = Some(error.to_string());

                        this.pending.clear();
                    }
                }

                cx.notify();
            });
        })
        .detach();

        entity
    }

    fn install(&mut self, store: RunStore) {
        let run = store.run().clone();

        if self.graph.is_none() {
            match Graph::new(run.definition().clone()) {
                Ok(graph) => self.graph = Some(graph),
                Err(errors) => self.error = errors.first().map(ToString::to_string),
            }
        }

        if self.slots.len() < run.slots().len() {
            self.slots.resize_with(run.slots().len(), || None);
        }

        self.run = Some(run);
        self.store = Some(store);
    }

    pub fn run(&self) -> Option<&RunRecord> {
        self.run.as_ref()
    }

    pub fn graph(&self) -> Option<&Graph> {
        self.graph.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Whether the run can be resumed now.
    pub fn resumable(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.state().is_resumable())
    }

    /// Whether the run is going and can be stopped.
    pub fn stoppable(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| matches!(run.state(), RunState::Running | RunState::Failing))
    }

    /// Whether `node` is running and its agent waits for the user.
    pub fn needs_input(&self, node: usize, cx: &App) -> bool {
        self.live_host(node)
            .is_some_and(|host| host.needs_input(cx))
    }

    /// The session of the slot that is running `node`, for drawing what the
    /// agent asks the user.
    pub fn node_session(&self, node: usize) -> Option<&Entity<AgentSession>> {
        self.live_host(node).map(SlotHost::session)
    }

    /// Entries of `node`'s running turn, read from its slot's conversation.
    pub fn live_entries(
        &self,
        node: usize,
        cx: &App,
    ) -> Option<Vec<TranscriptEntry<EntryMetadata>>> {
        let host = self.live_host(node)?;
        let live = host.live.as_ref()?;
        let state = host.session().read(cx).controller.borrow();
        let conversation = state.conversation().borrow();

        Some(turn_entries(&conversation, live.turn, &live.prompt))
    }

    /// The saved transcript of `node`'s turn, or only its sent text when the
    /// turn never ended, read once the changes queued before it are saved.
    pub fn saved_turn(
        &mut self,
        node: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<Option<Vec<TranscriptEntry<EntryMetadata>>>, RunStoreError>> {
        let read = self.queue(
            move |store| match store.transcript(node)? {
                Some(items) => Ok(Some(items)),
                None => Ok(store
                    .prompt(node)?
                    .map(|text| vec![Item::UserMessage { text: Some(text) }])),
            },
            |_, _, _| {},
            cx,
        );

        cx.spawn(async move |_, _| Ok(read.await?.map(|items| saved_entries(&items))))
    }

    fn slot_owner(&self, slot: usize) -> Option<&SessionOwner> {
        self.slots.get(slot)?.as_ref().map(|host| &host.owner)
    }

    fn live_host(&self, node: usize) -> Option<&SlotHost> {
        let slot = self.graph.as_ref()?.slot_of(node);

        self.slots
            .get(slot)?
            .as_ref()
            .filter(|host| host.live.as_ref().is_some_and(|live| live.node == node))
    }

    fn enqueue(&mut self, operation: Operation, cx: &mut Context<Self>) {
        self.pending.push_back(operation);
        self.start_next(cx);
    }

    fn start_next(&mut self, cx: &mut Context<Self>) {
        if self.store.is_some()
            && let Some(operation) = self.pending.pop_front()
        {
            operation(self, cx);
        }
    }

    /// Queue `work` on the store behind the changes already waiting, and
    /// resolve with its result once `apply` has seen it.
    fn queue<R: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut RunStore) -> Result<R, RunStoreError> + Send + 'static,
        apply: impl FnOnce(&mut Self, &Result<R, RunStoreError>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<R, RunStoreError>> {
        if self.load_failed {
            return Task::ready(Err(unavailable()));
        }

        let (sender, receiver) = oneshot::channel();

        self.enqueue(
            Box::new(move |this, cx| {
                this.with_store(
                    work,
                    move |this, result, cx| {
                        apply(this, &result, cx);

                        let _ = sender.send(result);
                    },
                    cx,
                );
            }),
            cx,
        );

        cx.spawn(async move |_, _| receiver.await.unwrap_or_else(|_| Err(unavailable())))
    }

    /// Run `work` on the store now. Only an operation holding its turn may
    /// call this: the store is taken for the duration.
    fn with_store<R: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut RunStore) -> Result<R, RunStoreError> + Send + 'static,
        apply: impl FnOnce(&mut Self, Result<R, RunStoreError>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let mut store = self.store.take().expect("one run change at a time");

        let task = self.executor.spawn(async move {
            let result = work(&mut store);

            (store, result)
        });

        cx.spawn(async move |this, cx| {
            let (store, result) = task.await;

            let _ = this.update(cx, |this, cx| {
                this.install(store);

                if let Err(error) = &result {
                    this.error = Some(error.to_string());
                }

                apply(this, result, cx);

                this.start_next(cx);

                cx.notify();
            });
        })
        .detach();
    }

    /// Look at the run again after something changed: a saved change, a
    /// session signal, or a session state change.
    fn schedule(&mut self, cx: &mut Context<Self>) {
        cx.notify();

        if self.closed || self.load_failed || self.graph.is_none() {
            return;
        }

        if self.scheduled {
            self.refresh_again = true;

            return;
        }

        self.scheduled = true;

        self.enqueue(Box::new(|this, cx| this.pump(cx)), cx);
    }

    /// Save what the slot sessions report, fail the nodes they cannot run,
    /// start the sessions ready nodes need, and send the nodes that can go.
    fn pump(&mut self, cx: &mut Context<Self>) {
        let (Some(run), Some(graph)) = (self.run.as_ref(), self.graph.as_ref()) else {
            self.scheduled = false;

            self.start_next(cx);

            return;
        };

        let observations: Vec<Option<SlotObservation>> = self
            .slots
            .iter()
            .enumerate()
            .map(|(slot, host)| {
                host.as_ref()
                    .map(|host| host.observe(run.slots()[slot].conversation.as_deref(), cx))
            })
            .collect();

        let ready: Vec<bool> = observations
            .iter()
            .map(|observation| observation.as_ref().is_some_and(|seen| seen.ready))
            .collect();

        let dispatch = schedule(graph, run, &ready);

        let mut conversations = Vec::new();
        let mut failures: Vec<Failure> = Vec::new();
        let mut starts = Vec::new();

        for (slot, observation) in observations.iter().enumerate() {
            let Some(observation) = observation else {
                continue;
            };

            if let Some(conversation) = &observation.conversation {
                conversations.push((slot, conversation.clone()));
            }

            if let Some(exited) = &observation.exited {
                failures.push((
                    exited.node,
                    Some(exited.epoch),
                    observation.failure.clone().unwrap_or_default(),
                ));
            }
        }

        for &slot in &dispatch.prepare {
            let failure = match observations.get(slot).and_then(Option::as_ref) {
                Some(observation) => observation.failure.clone(),
                None if find_profile(&run.definition().slots[slot].body.profile, cx).is_none() => {
                    Some(format!(
                        "the profile `{}` is not saved",
                        run.definition().slots[slot].body.profile.name
                    ))
                }
                None => {
                    starts.push(slot);

                    None
                }
            };

            if let Some(reason) = failure
                && let Some(node) = next_node(graph, run, slot)
            {
                failures.push((node, None, reason));
            }
        }

        // A session that exited took its running node with it.
        for host in self.slots.iter_mut().flatten() {
            if failures.iter().any(|(node, epoch, _)| {
                epoch.is_some() && host.live.as_ref().is_some_and(|live| live.node == *node)
            }) {
                host.live = None;
            }
        }

        let send = dispatch.send;

        let after = move |this: &mut Self, saved: bool, cx: &mut Context<Self>| {
            this.scheduled = false;

            if saved && !this.closed {
                for slot in starts {
                    this.start_slot(slot, cx);
                }

                for node in send {
                    this.send(node, cx);
                }
            }

            if this.refresh_again {
                this.refresh_again = false;

                this.schedule(cx);
            }
        };

        // Most looks find nothing to save; a streaming turn asks for one per
        // chunk, so those skip the store entirely.
        if conversations.is_empty() && failures.is_empty() {
            after(self, true, cx);

            self.start_next(cx);

            return;
        }

        let now = now();

        self.with_store(
            move |store| {
                for (slot, conversation) in conversations {
                    store.update(|run| run.record_conversation(slot, conversation))?;
                }

                for (node, epoch, reason) in failures {
                    store.update(|run| run.fail(node, epoch, reason, now))?;
                }

                Ok(())
            },
            move |this, result, cx| after(this, result.is_ok(), cx),
            cx,
        );
    }

    fn start_slot(&mut self, slot: usize, cx: &mut Context<Self>) {
        let Some(run) = &self.run else { return };

        let body = &run.definition().slots[slot].body;

        let Some(profile) = find_profile(&body.profile, cx) else {
            return;
        };

        let settings = slot_settings(&profile, &body.settings);
        let resumed = run.slots()[slot].conversation.clone();
        let kind = profile.kind;
        let owner = AgentSession::create(profile, run.workspace().clone(), None, cx);

        self.attach_slot_owner(slot, owner, kind, resumed, cx);

        if let Some(host) = &self.slots[slot] {
            host.start(settings, cx);
        }
    }

    fn attach_slot_owner(
        &mut self,
        slot: usize,
        owner: SessionOwner,
        kind: AgentKind,
        resumed: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let session = owner.session().clone();

        let events = cx.subscribe(&session, move |this, _, signal, cx| {
            this.on_execution(slot, signal, cx)
        });

        let changed = cx.observe(&session, |this, _, cx| this.schedule(cx));

        let content = cx.subscribe(&session, |_, _, _: &TranscriptChanged, cx| {
            cx.emit(TranscriptChanged);
        });

        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, || None);
        }

        self.slots[slot] = Some(SlotHost::new(
            owner,
            kind,
            resumed,
            vec![events, changed, content],
        ));
    }

    /// Compose and save `node`'s text, record it as sending, then send it.
    fn send(&mut self, node: usize, cx: &mut Context<Self>) {
        let (Some(run), Some(graph)) = (&self.run, &self.graph) else {
            return;
        };

        let slot = graph.slot_of(node);

        let Some(host) = self.slots.get(slot).and_then(Option::as_ref) else {
            return;
        };

        let epoch = host
            .session()
            .read(cx)
            .controller
            .borrow()
            .runtime()
            .epoch();

        let graph = graph.clone();
        let opened = run.slots()[slot].opened;
        let role = run.definition().slots[slot].body.role.clone();
        let input = run.input().to_owned();

        let title = if input.trim().is_empty() {
            run.definition_name().to_owned()
        } else {
            input.clone()
        };

        let now = now();

        self.enqueue(
            Box::new(move |this, cx| {
                this.with_store(
                    move |store| {
                        let mut read: BTreeSet<usize> = graph.needs(node).iter().copied().collect();

                        if let Some(template) = graph.template(node) {
                            read.extend(template.outputs().filter_map(|id| graph.node_index(id)));
                        }

                        let mut outputs = vec![None; graph.node_count()];

                        for index in read {
                            outputs[index] = store.output(index)?;
                        }

                        let role = (!opened).then_some(role.as_str());

                        let Some(text) = compose(&graph, node, &input, role, &outputs) else {
                            return Ok(None);
                        };

                        store.save_prompt(node, &text)?;

                        let began = store.update(|run| run.begin_send(node, epoch, now))?;

                        Ok(began.then_some(text))
                    },
                    move |this, result, cx| match result {
                        Ok(Some(text)) => this.submit(node, slot, epoch, text, &title, cx),
                        _ => this.schedule(cx),
                    },
                    cx,
                );
            }),
            cx,
        );
    }

    fn submit(
        &mut self,
        node: usize,
        slot: usize,
        epoch: u64,
        text: String,
        title: &str,
        cx: &mut Context<Self>,
    ) {
        let closed = self.closed;

        let outcome = match self.slots.get_mut(slot).and_then(Option::as_mut) {
            Some(host)
                if !closed
                    && host
                        .session()
                        .read(cx)
                        .controller
                        .borrow()
                        .runtime()
                        .epoch()
                        == epoch =>
            {
                let settings = host
                    .session()
                    .read(cx)
                    .controller
                    .borrow()
                    .controls
                    .settings
                    .clone();

                let outcome = host
                    .owner
                    .submit_prepared(text.clone(), title, &settings, cx);

                if outcome == SendOutcome::StartedTurn {
                    let turn = host.session().read(cx).controller.borrow().turn();

                    host.live = Some(LiveNode {
                        node,
                        epoch,
                        turn,
                        prompt: text,
                    });
                }

                outcome
            }
            _ => SendOutcome::NotReady,
        };

        let reason = match outcome {
            SendOutcome::StartedTurn => {
                self.schedule(cx);

                return;
            }
            SendOutcome::Steered => "the slot's agent was already running a turn".to_owned(),
            SendOutcome::NotReady => "the slot's agent was not ready to take the node".to_owned(),
            SendOutcome::Rejected { message } => message,
        };

        let now = now();

        self.queue(
            move |store| store.update(|run| run.fail(node, Some(epoch), reason, now)),
            |this, _, cx| this.schedule(cx),
            cx,
        )
        .detach();
    }

    fn on_execution(&mut self, slot: usize, signal: &ExecutionSignal, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }

        let Some(host) = self.slots.get_mut(slot).and_then(Option::as_mut) else {
            return;
        };

        let Some(live) = host.live.clone() else {
            return;
        };

        let now = now();

        match signal.clone() {
            ExecutionSignal::Accepted { epoch, id } if epoch == live.epoch => {
                let Some(graph) = self.graph.clone() else {
                    return;
                };

                self.queue(
                    move |store| store.update(|run| run.accept(&graph, live.node, epoch, &id)),
                    |this, _, cx| this.schedule(cx),
                    cx,
                )
                .detach();
            }
            ExecutionSignal::Finished {
                epoch,
                id,
                error,
                text,
            } if epoch == live.epoch => {
                let items = {
                    let state = host.session().read(cx).controller.borrow();
                    let conversation = state.conversation().borrow();

                    turn_items(&conversation, live.turn, &live.prompt)
                };

                host.live = None;

                let node = live.node;

                self.queue(
                    move |store| {
                        store.save_transcript(node, &items)?;

                        match error {
                            Some(reason) => {
                                store.update(|run| run.fail(node, Some(epoch), reason, now))
                            }
                            None => {
                                store.save_output(node, &text)?;

                                store.update(|run| run.complete(node, epoch, &id, now))
                            }
                        }
                    },
                    |this, _, cx| this.schedule(cx),
                    cx,
                )
                .detach();
            }
            _ => {}
        }
    }

    /// Stop the run: interrupt every running turn and start nothing more.
    /// The turns that were running keep the transcript they reached.
    pub fn stop(&mut self, cx: &mut Context<Self>) -> Task<Result<Vec<usize>, RunStoreError>> {
        let captured: Vec<(usize, Vec<Item>)> = self
            .slots
            .iter()
            .flatten()
            .filter_map(|host| {
                let live = host.live.as_ref()?;
                let state = host.session().read(cx).controller.borrow();
                let conversation = state.conversation().borrow();

                Some((
                    live.node,
                    turn_items(&conversation, live.turn, &live.prompt),
                ))
            })
            .collect();

        let now = now();

        self.queue(
            move |store| {
                for (node, items) in &captured {
                    store.save_transcript(*node, items)?;
                }

                store.update(|run| run.stop(now))
            },
            |this, result, cx| {
                if result.is_ok() {
                    for host in this.slots.iter_mut().flatten() {
                        if host.live.take().is_some() {
                            host.interrupt(cx);
                        }
                    }
                }

                this.schedule(cx);
            },
            cx,
        )
    }

    /// Resume a failed, stopped or interrupted run. Nodes that had not
    /// completed are sent again; work they already did is not undone.
    pub fn resume(&mut self, cx: &mut Context<Self>) -> Task<Result<bool, RunStoreError>> {
        // A slot whose session failed to start or exited would fail its next
        // node again, so it gets a new session that resumes its conversation.
        for slot in &mut self.slots {
            let unusable = slot.as_ref().is_some_and(|host| {
                let state = host.session().read(cx).controller.borrow();

                state.runtime().start_failure().is_some()
                    || state.runtime().status() == Status::Exited
            });

            if unusable && let Some(host) = slot.take() {
                host.owner.close();
            }
        }

        self.queue(
            |store| store.update(RunRecord::resume),
            |this, _, cx| this.schedule(cx),
            cx,
        )
    }

    /// Interrupt every running turn and release the slot sessions. The run
    /// stays saved as going, so reopening it marks it interrupted.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }

        self.closed = true;

        for host in self.slots.iter_mut().flatten() {
            if host.live.take().is_some() {
                host.interrupt(cx);
            }

            host.owner.close();
        }

        cx.notify();
    }
}

/// The first slot, in definition order, whose profile is not saved.
pub fn missing_profile(graph: &Graph, cx: &App) -> Option<MissingProfile> {
    graph
        .definition()
        .slots
        .iter()
        .find(|slot| find_profile(&slot.body.profile, cx).is_none())
        .map(|slot| MissingProfile {
            slot: slot.name.clone(),
            profile: slot.body.profile.clone(),
        })
}

fn find_profile(reference: &ProfileReference, cx: &App) -> Option<AgentProfile> {
    cx.global::<AgentSettings>()
        .profiles
        .iter()
        .find(|profile| profile.kind == reference.kind && profile.name == reference.name)
        .cloned()
}

/// The profile's pinned controls with the slot's own values over them.
fn slot_settings(profile: &AgentProfile, overrides: &ThreadSettings) -> ThreadSettings {
    let pins = launch_pins(profile.kind, profile);

    ThreadSettings {
        model: overrides.model.clone().or(pins.model),
        approval: overrides.approval.clone().or(pins.approval),
        approvals_reviewer: overrides.approvals_reviewer.clone(),
        sandbox: overrides.sandbox.clone().or(pins.sandbox),
        effort: overrides.effort.clone().or(pins.effort),
        tier: overrides.tier.clone(),
        agent_preset: overrides.agent_preset.clone(),
    }
}

/// The waiting node `slot` would run next: dependencies completed, earliest
/// in graph order.
fn next_node(graph: &Graph, run: &RunRecord, slot: usize) -> Option<usize> {
    graph.order().iter().copied().find(|&node| {
        graph.slot_of(node) == slot
            && run.nodes()[node].state == NodeState::Waiting
            && graph
                .needs(node)
                .iter()
                .all(|&dependency| run.nodes()[dependency].state.is_completed())
    })
}

fn unavailable() -> RunStoreError {
    RunStoreError::Io(io::Error::other("the orchestration run is closed"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}
