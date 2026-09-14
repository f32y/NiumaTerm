//! Session execution and logical lifetime, independent of presentation entities.

pub use crate::agent_tab::execution::children::ChildReader;
pub use crate::agent_tab::execution::registry::SessionRegistry;

pub(super) mod inbox;

mod children;
mod registry;

#[cfg(test)]
mod tests;

use crate::agent_tab::composer::attachments::scratch_dir;
use crate::agent_tab::execution::children::ChildReaders;
use crate::agent_tab::execution::inbox::{
    EventBatch, MAX_MESSAGES_PER_BATCH, MAX_UPDATE_TIME, channel,
};
use crate::agent_tab::profile::{AgentKind, agent_launch};
use crate::agent_tab::session::RestorationReadiness;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::thread_controls::{launch_effort, launch_model, stored_thread_settings};
use crate::agent_tab::{AgentPaneEvent, RecoveryReadiness};
use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use futures::stream::ReadyChunks;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, Task, WeakEntity};
use nmt_agent::background_task::BackgroundTaskKey;
use nmt_agent::chat::{Event, Item, QuestionMode, SlashCommandOutcome, TeamDecisionRequest};
use nmt_agent::claude_code::sessions;
use nmt_agent::launcher::AgentCli;
use nmt_agent::session::branch::{BranchUpdate, CheckpointRead};
use nmt_agent::session::capabilities::AgentCapabilities as _;
use nmt_agent::session::controller::{
    QuestionSubmission, ReadyDefaults, SessionController, SessionEffect,
};
use nmt_agent::session::input::QuestionAction;
use nmt_agent::session::lifecycle::{RecoverySnapshot, StartOutcome};
use nmt_agent::session::restore::{ReplayLoaded, ReplayRead, SettingsSeed};
use nmt_agent::session::team_capabilities::TeamLaunch;
use nmt_agent::session::update_readiness::{ConversationWork, Readiness};
use nmt_agent::session::workflows::RefreshPlan;
use nmt_agent::session::{Backend, RecoveryIdentity};
use nmt_agent::update::InstallationKey;
use nmt_agent::workflow::{WorkflowRefreshRequest, WorkflowRefreshResult, WorkflowRun};
use nmt_agent::{
    AgentEvent, AgentEventKind, AgentRoute, AgentWorkspace, agent_process, normalize_body,
    normalize_title,
};
use nmt_config::profile::{AgentProfile, AgentProfileKind};
use rust_i18n::t;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::{fs, thread};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(Uuid);

#[derive(Default)]
pub(super) struct PollingRefresh<R> {
    task: Option<Task<()>>,
    pub(super) readers: R,
}

pub struct AgentSession {
    child_refresh: PollingRefresh<ChildReaders>,
    pub(super) workflow_refresh: PollingRefresh<Rc<Cell<usize>>>,
    pub(super) controller: Rc<RefCell<SessionController>>,

    /// The launch profile this pane was opened with (executable, endpoint,
    /// env vars); every session (re)start uses it.
    pub(super) profile: AgentProfile,

    /// The directories this tab is configured with. The primary one is the
    /// tab's working directory: the session process runs there and provider
    /// session history is scoped to it, because a resume id only resolves
    /// against the directory its conversation ran in. Editing the parent
    /// workspace replaces this list for the next conversation.
    pub(super) workspace: AgentWorkspace,

    /// The directory list the running conversation was started with. Held
    /// apart from `workspace` so an edit never changes what a process already
    /// running was granted.
    pub(super) active_workspace: AgentWorkspace,

    pub(super) route: AgentRoute,
    pub(super) kind: AgentKind,
    id: SessionId,
    pub(super) last_completed: Option<(u64, u64)>,
    closed: Rc<Cell<bool>>,
    binding_generation: Rc<Cell<u64>>,
    pub(super) team_launch: Option<TeamLaunch>,
}

/// Closing this owner releases execution even while observers still exist.
pub struct SessionOwner {
    session: Entity<AgentSession>,
    controller: Rc<RefCell<SessionController>>,
    closed: Rc<Cell<bool>>,
    binding_generation: Rc<Cell<u64>>,
    id: SessionId,
    registry: Rc<RefCell<HashMap<SessionId, WeakEntity<AgentSession>>>>,
    scratch: PathBuf,
}

pub(super) struct CommandBinding {
    closed: Rc<Cell<bool>>,
    current: Rc<Cell<u64>>,
    pub(super) generation: u64,
}

impl CommandBinding {
    pub(super) fn is_current(&self) -> bool {
        !self.closed.get() && self.current.get() == self.generation
    }
}

impl Drop for CommandBinding {
    fn drop(&mut self) {
        if self.is_current() {
            self.current.set(self.generation.wrapping_add(1));
        }
    }
}

pub(super) struct PresentationEffect {
    pub(super) epoch: u64,
    pub(super) generation: u64,
    pub(super) effect: RefCell<Option<SessionEffect>>,
}

impl EventEmitter<PresentationEffect> for AgentSession {}

impl EventEmitter<AgentPaneEvent> for AgentSession {}

#[derive(Clone)]
pub(super) enum ExecutionSignal {
    Accepted {
        epoch: u64,
        id: String,
    },

    Finished {
        epoch: u64,
        id: String,
        error: Option<String>,
        text: String,
    },

    Decision {
        epoch: u64,
        request: TeamDecisionRequest,
    },
}

impl EventEmitter<ExecutionSignal> for AgentSession {}

impl SessionOwner {
    pub fn session(&self) -> &Entity<AgentSession> {
        &self.session
    }

    pub fn start(&self, recovery: Option<RecoveryIdentity>, cx: &mut App) {
        self.session.update(cx, |session, cx| {
            if session.controller.borrow().runtime.epoch() == 0 {
                session.start(recovery, false, |_, _| {}, cx);
            }
        });
    }

    pub(super) fn bind(&self) -> CommandBinding {
        self.binding_generation
            .set(self.binding_generation.get() + 1);

        CommandBinding {
            closed: self.closed.clone(),
            current: self.binding_generation.clone(),
            generation: self.binding_generation.get(),
        }
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }

        self.registry.borrow_mut().remove(&self.id);

        self.binding_generation
            .set(self.binding_generation.get() + 1);

        let mut controller = self.controller.borrow_mut();

        controller.starting(None);

        let backend = controller.runtime.retire();

        controller.failed("session closed", true);
        controller.clear_conversation();

        let scratch = self.scratch.clone();

        thread::spawn(move || {
            if let Some(mut backend) = backend {
                let _ = backend.shutdown(Duration::from_secs(5), true);
            }

            let _ = fs::remove_dir_all(scratch);
        });
    }
}

impl Drop for SessionOwner {
    fn drop(&mut self) {
        self.close();
    }
}

impl AgentSession {
    pub(super) fn create_team(
        profile: AgentProfile,
        workspace: AgentWorkspace,
        policy: TeamLaunch,
        cx: &mut App,
    ) -> SessionOwner {
        Self::create(profile, workspace, Some(policy), cx)
    }

    pub fn create(
        profile: AgentProfile,
        workspace: AgentWorkspace,
        team_launch: Option<TeamLaunch>,
        cx: &mut App,
    ) -> SessionOwner {
        let kind = profile.kind;
        let controller = Rc::new(RefCell::new(SessionController::new(kind)));
        let closed = Rc::new(Cell::new(false));
        let binding_generation = Rc::new(Cell::new(0));

        let session = cx.new(|_| Self {
            controller: controller.clone(),
            profile,
            active_workspace: workspace.clone(),
            workspace,
            route: agent_process().allocate_route(),
            kind,
            id: SessionId(Uuid::new_v4()),
            last_completed: None,
            workflow_refresh: PollingRefresh::default(),
            child_refresh: PollingRefresh::default(),
            closed: closed.clone(),
            binding_generation: binding_generation.clone(),
            team_launch,
        });

        let registry = cx.default_global::<SessionRegistry>().0.clone();
        let id = session.read(cx).id;
        let scratch = scratch_dir(session.read(cx).route.as_str());

        registry.borrow_mut().insert(id, session.downgrade());

        SessionOwner {
            session,
            controller,
            closed,
            binding_generation,
            id,
            registry,
            scratch,
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    pub fn agent_route(&self) -> &AgentRoute {
        &self.route
    }

    pub fn profile(&self) -> &AgentProfile {
        &self.profile
    }

    pub fn background_task_count(&self) -> usize {
        self.controller
            .borrow()
            .background_tasks()
            .map_or(0, |snapshot| snapshot.tasks.len())
    }

    pub fn downgrade(owner: &SessionOwner) -> WeakEntity<Self> {
        owner.session.downgrade()
    }

    pub(super) fn publish(&self, effect: SessionEffect, cx: &mut Context<Self>) {
        let epoch = self.controller.borrow().runtime.epoch();

        match &effect {
            SessionEffect::ProviderTurnAccepted { id } => cx.emit(ExecutionSignal::Accepted {
                epoch,
                id: id.clone(),
            }),

            SessionEffect::ProviderTurnFinished { id, error } => {
                let state = self.controller.borrow();

                let text = state
                    .conversation
                    .borrow()
                    .content
                    .latest_agent_message(state.delivery.turn())
                    .unwrap_or_default()
                    .to_owned();

                cx.emit(ExecutionSignal::Finished {
                    epoch,
                    id: id.clone(),
                    error: error.clone(),
                    text,
                });
            }

            SessionEffect::TeamDecision(request) => cx.emit(ExecutionSignal::Decision {
                epoch,
                request: request.clone(),
            }),

            _ => {}
        }

        cx.emit(PresentationEffect {
            epoch,
            generation: self.binding_generation.get(),
            effect: RefCell::new(Some(effect)),
        });

        cx.notify();
    }

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

                state.begin_branched_conversation();
                drop(state);
                self.publish(SessionEffect::Branch(BranchUpdate::Branching), cx);
            }

            update => self.publish(SessionEffect::Branch(update), cx),
        }
    }

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
                    this.on_event(epoch, event, cx);
                }
            });
        })
        .detach();
    }

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
            let mut readers = self.child_refresh.readers.borrow_mut();
            let count = readers.entry(reader_key.clone()).or_default();

            *count += 1;

            *count == 1
        };

        if first {
            self.load_child(key, cx);
        }

        if self.child_refresh.task.is_none() {
            self.child_refresh.task = Some(cx.spawn(Self::poll_watched_children));
        }

        Some(ChildReader {
            key: reader_key,
            readers: self.child_refresh.readers.clone(),
        })
    }

    async fn poll_watched_children(this: WeakEntity<Self>, cx: &mut AsyncApp) {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;

            let alive = this.update(cx, |this, cx| {
                if this.is_closed() || this.child_refresh.readers.borrow().is_empty() {
                    return false;
                }

                let keys: Vec<_> = this
                    .child_refresh
                    .readers
                    .borrow()
                    .keys()
                    .cloned()
                    .collect();

                for (epoch, key) in keys {
                    if !this.controller.borrow().runtime.is_current(epoch) {
                        continue;
                    }

                    let active =
                        this.controller
                            .borrow()
                            .background_tasks()
                            .is_some_and(|snapshot| {
                                snapshot
                                    .tasks
                                    .iter()
                                    .any(|task| task.key == key && task.state.is_active())
                            });

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

        let _ = this.update(cx, |this, _| this.child_refresh.task = None);
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
            self.on_event(epoch, event, cx);
        }
    }

    pub(crate) fn on_event(&mut self, epoch: u64, event: Event, cx: &mut Context<Self>) {
        if self.is_closed() || !self.controller.borrow().runtime.is_current(epoch) {
            return;
        }

        self.prepare_defaults(cx);

        let event = match event {
            Event::HostExited { message } => {
                let mut state = self.controller.borrow_mut();

                let identity = state
                    .runtime
                    .backend()
                    .and_then(|backend| backend.recovery_identity());

                state.runtime.reconnect(Some(RecoverySnapshot {
                    identity,
                    profile_name: self.profile.name.clone(),
                }));

                state.runtime.recovery_failed(message.clone());

                Event::Error {
                    message,
                    fatal: true,
                }
            }

            event => event,
        };

        let effect = self.controller.borrow_mut().apply_event(epoch, event);

        if let SessionEffect::Branch(update) = effect {
            self.on_branch_update(update, cx);

            return;
        }

        if let SessionEffect::StatusDetail(detail) = &effect {
            let detail = detail.as_ref().map(|retry| {
                t!(
                    "agent-transcript-retrying",
                    attempt = retry.attempt,
                    total = retry.total,
                    reason = &retry.reason
                )
                .into_owned()
            });

            let state = self.controller.borrow();
            let mut conversation = state.conversation.borrow_mut();

            conversation.live.set_detail(detail);
            conversation.changed_turn(state.delivery.turn());
        }

        self.publish_activity(&effect, cx);

        match &effect {
            SessionEffect::Ready(_) => {
                self.restore_background_tasks(cx);
                self.restore_workflows(cx);
            }

            SessionEffect::InputRequested { index } => self.expire_optional_question(*index, cx),
            SessionEffect::Workflows { .. } => self.sync_workflow_refresh(cx),
            _ => {}
        }

        self.publish(effect, cx);
        self.advance_commands(cx);
    }

    pub(crate) fn advance_commands(&mut self, cx: &mut Context<Self>) {
        if self.is_closed() {
            return;
        }

        loop {
            let next = self.controller.borrow_mut().next_command();

            let Some((name, outcome)) = next else {
                break;
            };

            let accepted = matches!(outcome, SlashCommandOutcome::Accepted);

            self.publish(
                SessionEffect::CommandResult {
                    name,
                    outcome,
                    advance: false,
                },
                cx,
            );

            if accepted {
                break;
            }
        }
    }

    fn publish_activity(&mut self, effect: &SessionEffect, cx: &mut Context<Self>) {
        match effect {
            SessionEffect::Title(title) => cx.emit(AgentPaneEvent::TitleSuggested(title.clone())),

            SessionEffect::TurnStarted { .. } => {
                self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx)
            }

            SessionEffect::TurnCompleted { error, .. } => {
                let state = self.controller.borrow();
                let key = (state.runtime.epoch(), state.delivery.turn());

                if self.last_completed == Some(key) {
                    return;
                }

                let body = error
                    .clone()
                    .or_else(|| {
                        state
                            .conversation
                            .borrow()
                            .content
                            .latest_agent_message(key.1)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| {
                        t!("agent-session-turn-completed", name = self.kind.display()).into_owned()
                    });

                drop(state);
                self.last_completed = Some(key);

                self.emit_lifecycle(
                    AgentEventKind::Stopped,
                    &t!(
                        "agent-session-provider-finished",
                        name = self.kind.display()
                    ),
                    &body,
                    cx,
                );
            }

            SessionEffect::ApprovalRequested => {
                let body = self
                    .controller
                    .borrow()
                    .input
                    .approval()
                    .unwrap_or_default()
                    .to_string();

                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &t!("agent-session-needs-input", name = self.kind.display()),
                    &body,
                    cx,
                );
            }

            SessionEffect::InputRequested { index } => {
                let state = self.controller.borrow();
                let prompt = &state.input.batches()[*index];

                if prompt.mode() == QuestionMode::Async {
                    return;
                }

                let body = prompt
                    .questions()
                    .first()
                    .map_or("", |question| question.question.as_str())
                    .to_string();

                drop(state);

                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &t!("agent-session-needs-input", name = self.kind.display()),
                    &body,
                    cx,
                );
            }

            SessionEffect::ApprovalResolved => {
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx)
            }

            SessionEffect::InputResolved(completion) => {
                if completion.started_turn {
                    self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
                }

                if completion.waiting_finished {
                    self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                }
            }

            SessionEffect::Workflows {
                activity_changed: true,
            } => cx.emit(AgentPaneEvent::WorkflowActivity),

            SessionEffect::BackgroundActivity => cx.emit(AgentPaneEvent::BackgroundTaskActivity),
            SessionEffect::Error { fatal: true, .. } => cx.emit(AgentPaneEvent::Interrupted),
            _ => {}
        }
    }

    pub(crate) fn emit_lifecycle(
        &self,
        kind: AgentEventKind,
        title: &str,
        body: &str,
        cx: &mut Context<Self>,
    ) {
        let state = self.controller.borrow();
        let agent: &str = self.kind.into();

        cx.emit(AgentPaneEvent::Lifecycle(AgentEvent {
            route: self.route.clone(),
            agent: agent.into(),
            session_id: self.id.0.to_string(),
            turn_id: (kind != AgentEventKind::SessionStarted)
                .then(|| format!("turn-{}", state.delivery.turn())),
            kind,
            title: normalize_title(title),
            body: normalize_body(body),
        }));
    }

    pub(crate) fn read_resume(&mut self, request: ReplayRead, cx: &mut Context<Self>) {
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

                        this.on_event(
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

    /// `None` when this tab's harness has no vendor-managed installation, which
    /// is also what keeps such a tab out of every update transaction: it
    /// matches no installation being updated.
    pub fn installation_key(&self) -> Option<InstallationKey> {
        let provider = self.kind.provider_kind()?;
        let launch = agent_launch(&self.profile);
        let launcher = AgentCli::from_launch(&launch, provider.default_executable());

        Some(InstallationKey::derive(provider, &launcher).key)
    }

    /// Assess both quiescence and recoverability before any related backend
    /// is stopped. A blank tab needs no provider identity because restarting
    /// it as another blank conversation loses no conversation state.
    fn update_work(&self, _cx: &App) -> ConversationWork {
        ConversationWork {
            approval_open: self.controller.borrow().input.approval().is_some(),
            branch_pending: self.controller.borrow().branch.holds_composer(),
            compacting: self
                .controller
                .borrow()
                .conversation
                .borrow()
                .live
                .is_compacting(),
            empty: self
                .controller
                .borrow()
                .conversation
                .borrow()
                .content
                .entries()
                .is_empty(),
        }
    }

    pub fn recovery_readiness(&self, cx: &App) -> RecoveryReadiness {
        self.present_readiness(
            self.controller
                .borrow()
                .update_readiness(self.update_work(cx)),
        )
    }

    pub fn recovery_identity_snapshot(&self, cx: &App) -> RecoveryReadiness {
        self.present_readiness(
            self.update_work(cx)
                .identity(self.controller.borrow().runtime.backend()),
        )
    }

    fn present_readiness(&self, readiness: Readiness) -> RecoveryReadiness {
        match readiness {
            Readiness::Ready(identity) => RecoveryReadiness::Ready(RecoverySnapshot {
                identity,
                profile_name: self.profile.name.clone(),
            }),

            Readiness::Updating => RecoveryReadiness::Busy(
                t!(
                    "agent-update-profile-already-updating",
                    name = &self.profile.name
                )
                .into_owned(),
            ),

            Readiness::ActiveWork => RecoveryReadiness::Busy(
                t!(
                    "agent-update-profile-active-work",
                    name = &self.profile.name
                )
                .into_owned(),
            ),

            Readiness::MissingIdentity => RecoveryReadiness::MissingIdentity(
                t!(
                    "agent-update-profile-missing-identity",
                    name = &self.profile.name,
                    provider = self.kind.display()
                )
                .into_owned(),
            ),
        }
    }

    pub fn prepare_update_wait(&mut self, cx: &mut Context<Self>) {
        self.controller.borrow_mut().runtime.wait_for_update();

        cx.notify();
    }

    pub fn cancel_update_wait(&mut self, cx: &mut Context<Self>) {
        if self.controller.borrow_mut().runtime.cancel_update_wait() {
            cx.notify();
        }
    }

    pub fn stop_active_work_for_update(&mut self, cx: &mut Context<Self>) {
        if self.controller.borrow().input.approval().is_some() {
            self.controller.borrow_mut().respond_approval("cancel");
        } else {
            self.controller.borrow_mut().runtime.interrupt(None);
        }

        self.controller.borrow_mut().prepare_update_stop();
        self.controller.borrow_mut().publish_confirmed();

        self.controller.borrow_mut().branch.cancel_picker();

        self.controller
            .borrow()
            .conversation
            .borrow_mut()
            .live
            .set_compacting(false);

        self.controller.borrow_mut().runtime.wait_for_update();

        cx.notify();
    }

    /// Detach the backend before shutdown so its EOF cannot be mistaken for
    /// an unexpected pane exit. The transcript, draft, selection, scroll, and
    /// thread controls remain owned by this entity throughout the operation.
    pub fn suspend_for_update(
        &mut self,
        force: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        if self.is_closed() {
            return Task::ready(Ok(()));
        }

        let (epoch, backend) = self.controller.borrow_mut().runtime.suspend_for_update();

        cx.emit(AgentPaneEvent::Interrupted);
        cx.notify();

        let Some(mut backend) = backend else {
            return Task::ready(Ok(()));
        };

        let worker = cx.background_executor().spawn(async move {
            let result = backend.shutdown(Duration::from_secs(5), force);

            (backend, result)
        });

        cx.spawn(async move |this, cx| Self::finish_suspension(this, worker, epoch, cx).await)
    }

    async fn finish_suspension(
        this: WeakEntity<Self>,
        worker: Task<(Backend, Result<(), String>)>,
        epoch: u64,
        cx: &mut AsyncApp,
    ) -> Result<(), String> {
        let (backend, result) = worker.await;

        if result.is_err() {
            let _ = this.update(cx, |this, cx| {
                if let Err(mut orphan) = this
                    .controller
                    .borrow_mut()
                    .runtime
                    .shutdown_failed(epoch, backend)
                {
                    cx.background_executor()
                        .spawn(async move {
                            let _ = orphan.shutdown(Duration::from_secs(5), true);
                        })
                        .detach();
                }

                cx.notify();
            });
        }

        result
    }

    pub fn mark_provider_updating(&mut self, cx: &mut Context<Self>) {
        self.controller.borrow_mut().runtime.provider_updating();

        cx.notify();
    }

    /// The outcome reaches the caller through [`Self::restoration_readiness`]:
    /// the process now comes up on a background thread, so a failure lands
    /// after this returns.
    pub fn restore_after_update(&mut self, snapshot: &RecoverySnapshot, cx: &mut Context<Self>) {
        if self.is_closed() {
            return;
        }

        self.controller
            .borrow_mut()
            .runtime
            .reconnect(Some(snapshot.clone()));

        self.start(snapshot.identity.clone(), true, |_, _| {}, cx);

        cx.notify();
    }

    pub(crate) fn retry_update_recovery(&mut self, cx: &mut Context<Self>) {
        let snapshot = self
            .controller
            .borrow()
            .runtime
            .last_recovery_snapshot()
            .cloned();

        if let Some(snapshot) = snapshot {
            self.restore_after_update(&snapshot, cx);
        }
    }

    pub fn restoration_readiness(&self) -> RestorationReadiness {
        self.controller.borrow().runtime.restoration_readiness()
    }

    pub fn fail_update_recovery(&mut self, message: String, cx: &mut Context<Self>) {
        self.controller
            .borrow_mut()
            .runtime
            .recovery_failed(message);

        cx.notify();
    }

    pub(crate) fn start_new_after_update_failure(&mut self, cx: &mut Context<Self>) {
        self.controller.borrow_mut().runtime.reconnect(None);

        self.start(None, true, |_, _| {}, cx);

        cx.notify();
    }

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

    pub(crate) fn reset(&mut self, cx: &mut Context<Self>) {
        if self.is_closed() {
            return;
        }

        let retiring = self.controller.borrow_mut().reset_for_restart();

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
                    session.controls.set_model(model);
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

            session.controls.seed_settings(seed);

            let restored = preserve_settings.then(|| session.controls.settings.clone());

            session.controls.restore_settings_on_ready(restored);

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
        let batches = receiver.ready_chunks(MAX_MESSAGES_PER_BATCH);

        let team_launch = self.team_launch.clone().map(|mut policy| {
            policy.restore_transcript = self
                .controller
                .borrow()
                .conversation
                .borrow()
                .content
                .entries()
                .is_empty();

            policy
        });

        let spawned = cx.background_executor().spawn(async move {
            if let Some(policy) = team_launch {
                return Backend::spawn_team(
                    kind,
                    &launch,
                    &catalog,
                    &workspace,
                    recovery,
                    policy,
                    move |message| sender.send(message),
                );
            }

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
            Self::pump_backend_messages(this, batches, spawned, epoch, name, on_result, cx).await
        })
        .detach();

        cx.notify();
    }

    async fn pump_backend_messages(
        this: WeakEntity<Self>,
        mut batches: ReadyChunks<UnboundedReceiver<Result<Value, String>>>,
        spawned: Task<Result<Backend, String>>,
        epoch: u64,
        name: &str,
        on_result: impl FnOnce(bool, &mut Context<Self>),
        cx: &mut AsyncApp,
    ) {
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

                            let message = match message {
                                Ok(message) => message,

                                Err(error) => {
                                    events.flush(|event| this.on_event(epoch, event, cx));

                                    if this.controller.borrow().runtime.is_current(epoch) {
                                        this.stop_for_output_failure(error, cx);
                                    }

                                    return false;
                                }
                            };

                            let next = this.controller.borrow_mut().runtime.process(epoch, message);

                            let Some(next) = next else {
                                return false;
                            };

                            for event in next {
                                events.push(event, |event| this.on_event(epoch, event, cx));
                            }

                            if started.elapsed() >= MAX_UPDATE_TIME {
                                break;
                            }
                        }

                        events.flush(|event| this.on_event(epoch, event, cx));

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
                this.on_event(epoch, event, cx);
            }

            if !this.controller.borrow().runtime.is_current(epoch) {
                return;
            }

            this.on_event(
                epoch,
                Event::Error {
                    message: t!("agent-session-exited", name = name).into_owned(),
                    fatal: true,
                },
                cx,
            );
        });
    }

    pub(crate) fn prepare_defaults(&self, cx: &Context<Self>) {
        let mut session = self.controller.borrow_mut();

        session.ready_defaults = match session.controls.seed {
            SettingsSeed::Defaults => ReadyDefaults {
                stored: stored_thread_settings(self.kind, &self.profile, cx).cloned(),
                model: launch_model(self.kind, &self.profile),
                effort: launch_effort(&self.profile),
            },

            SettingsSeed::Reviewer => ReadyDefaults {
                stored: stored_thread_settings(self.kind, &self.profile, cx).cloned(),
                ..ReadyDefaults::default()
            },

            SettingsSeed::None => ReadyDefaults::default(),
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
            t!("agent-session-start-failed", name = name, error = &error).into_owned()
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
                self.on_event(epoch, event, cx);
            }

            cx.background_executor()
                .spawn(async move {
                    let _ = backend.shutdown(Duration::from_millis(250), true);
                })
                .detach();
        }

        self.on_event(
            epoch,
            Event::Error {
                message: error,
                fatal: true,
            },
            cx,
        );
    }

    /// Read every completed run this session already recorded. A resumed
    /// conversation replays nothing, so its finished runs exist only on disk.
    ///
    /// Runs once per session, from whichever comes first: the session becoming
    /// ready, or the view opening. The ready path is what lets a resumed
    /// conversation surface the title-bar control at all — the view cannot be
    /// opened before the control exists, so waiting for it would strand every
    /// run recorded before this tab opened.
    pub(crate) fn restore_workflows(&mut self, cx: &mut Context<Self>) {
        // A harness that reports its runs live replays them with the rest of
        // the conversation, so there is no stored record to go looking for.
        let source = self
            .controller
            .borrow()
            .runtime
            .backend()
            .and_then(Backend::workflow_source);

        let Some(source) = source else {
            return;
        };

        let Some(session_id) = self
            .controller
            .borrow()
            .runtime
            .backend()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };

        if !self
            .controller
            .borrow_mut()
            .workflows
            .claim_restore(&session_id)
        {
            return;
        }

        let cwd = self.active_workspace.primary().map(str::to_owned);
        let epoch = self.controller.borrow().runtime.epoch();

        let read = cx
            .background_executor()
            .spawn(async move { source.restore(cwd.as_deref(), &session_id) });

        cx.spawn(async move |this, cx| {
            let restored = read.await;

            this.update(cx, |this, cx| {
                // A restoration that outlived its session says nothing about
                // the conversation now open.
                if this.is_closed() || !this.controller.borrow().runtime.is_current(epoch) {
                    return;
                }

                this.merge_restored_workflows(restored, cx);
            })
            .ok();
        })
        .detach();
    }

    fn merge_restored_workflows(
        &mut self,
        restored: Result<Vec<WorkflowRun>, String>,
        cx: &mut Context<Self>,
    ) {
        // A failed read leaves whatever the live stream reported; the view is
        // still usable and the next open retries.
        let Ok(restored) = restored else {
            self.controller.borrow_mut().workflows.forget_restore();

            return;
        };

        let events = {
            let mut state = self.controller.borrow_mut();

            let Some(session) = state.runtime.backend_mut() else {
                return;
            };

            session.restore_workflows(restored)
        };

        for event in events {
            let epoch = self.controller.borrow().runtime.epoch();

            self.on_event(epoch, event, cx);
        }
    }

    /// Start the poll when there is something to poll, stop it otherwise.
    pub(crate) fn sync_workflow_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.should_refresh_workflows() {
            self.workflow_refresh.task = None;

            return;
        }

        if self.workflow_refresh.task.is_some() {
            return;
        }

        self.workflow_refresh.task = Some(cx.spawn(Self::poll_workflow_refresh));
    }

    async fn poll_workflow_refresh(this: WeakEntity<Self>, cx: &mut AsyncApp) {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;

            let Ok(Some(plan)) = this.update(cx, |this, _| this.workflow_refresh_plan()) else {
                break;
            };

            let epoch = plan.epoch;

            // A tick does its own reads before the next beat, so ticks can
            // fall behind but never overlap or queue up.
            let results = cx
                .background_executor()
                .spawn(async move { plan.read() })
                .await;

            let applied = this.update(cx, |this, cx| {
                this.on_workflow_refresh_results(epoch, results, cx)
            });

            if !matches!(applied, Ok(true)) {
                break;
            }
        }

        let _ = this.update(cx, |this, _| this.workflow_refresh.task = None);
    }

    fn should_refresh_workflows(&self) -> bool {
        !self.is_closed()
            && self.workflow_refresh.readers.get() > 0
            && self
                .controller
                .borrow()
                .runtime
                .backend()
                .and_then(Backend::workflow_source)
                .is_some()
            && self.controller.borrow().workflows.has_active_run()
    }

    fn workflow_refresh_plan(&self) -> Option<RefreshPlan> {
        if !self.should_refresh_workflows() {
            return None;
        }

        self.controller.borrow().workflows.refresh_plan(
            &self.controller.borrow().runtime,
            self.active_workspace.primary().map(str::to_owned),
        )
    }

    /// Fold a tick's reads in. Returns whether the loop should keep running.
    fn on_workflow_refresh_results(
        &mut self,
        epoch: u64,
        results: Vec<WorkflowRefreshResult>,
        cx: &mut Context<Self>,
    ) -> bool {
        // A tick that outlived its session must not touch the new one.
        if self.is_closed() || !self.controller.borrow().runtime.is_current(epoch) {
            return false;
        }

        for result in results {
            self.controller
                .borrow_mut()
                .workflows
                .accept_revision(&result);

            let events = {
                let mut state = self.controller.borrow_mut();

                let Some(session) = state.runtime.backend_mut() else {
                    return false;
                };

                session.apply_workflow_refresh(result)
            };

            for event in events {
                let epoch = self.controller.borrow().runtime.epoch();

                self.on_event(epoch, event, cx);
            }
        }

        if self
            .controller
            .borrow_mut()
            .workflows
            .mark_open_availability()
        {
            cx.notify();
        }

        self.should_refresh_workflows()
    }

    /// Read the open conversation once, outside the tick cadence.
    pub(crate) fn read_open_workflow_agent(
        &mut self,
        task_id: &str,
        agent_id: &str,
        cx: &mut Context<Self>,
    ) {
        let task_id = task_id.to_owned();
        let agent_id = agent_id.to_owned();

        // A harness that reports its runs live has no stored record to read:
        // the member is a conversation of its own on the host, and asking for
        // it is one request whose answer arrives as an ordinary event.
        let source = self
            .controller
            .borrow()
            .runtime
            .backend()
            .and_then(Backend::workflow_source);

        let Some(source) = source else {
            if let Some(session) = self.controller.borrow_mut().runtime.backend_mut() {
                session.request_workflow_agent_transcript(&task_id, &agent_id);
            }

            return;
        };

        let Some(session_id) = self
            .controller
            .borrow()
            .runtime
            .backend()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };

        let request = WorkflowRefreshRequest {
            task_id: task_id.clone(),
            agent_ids: self.controller.borrow().workflows.agent_ids(&task_id),
            open_agent: Some(agent_id),
            transcript_revision: None,
        };

        let cwd = self.active_workspace.primary().map(str::to_owned);
        let epoch = self.controller.borrow().runtime.epoch();

        let read = cx
            .background_executor()
            .spawn(async move { source.refresh(cwd.as_deref(), &session_id, &request) });

        cx.spawn(async move |this, cx| {
            let result = read.await;

            this.update(cx, |this, cx| {
                if this.is_closed() || !this.controller.borrow().runtime.is_current(epoch) {
                    return;
                }

                this.on_workflow_refresh_results(epoch, vec![result], cx);
            })
            .ok();
        })
        .detach();
    }
}
