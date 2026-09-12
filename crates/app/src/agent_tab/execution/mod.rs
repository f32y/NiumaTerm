//! Session execution and logical lifetime, independent of presentation entities.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use std::{fs, thread};

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Task, WeakEntity};
use nmt_agent::background_task::BackgroundTaskKey;
use nmt_agent::chat::TeamDecisionRequest;
use nmt_agent::session::RecoveryIdentity;
use nmt_agent::session::controller::{SessionController, SessionEffect};
use nmt_agent::session::team_capabilities::TeamLaunch;
use nmt_agent::{AgentRoute, AgentWorkspace, agent_process};
use nmt_config::profile::AgentProfile;
use uuid::Uuid;

use crate::agent_tab::AgentPaneEvent;
use crate::agent_tab::composer::attachments::scratch_dir;
pub use crate::agent_tab::execution::children::ChildReader;
pub use crate::agent_tab::execution::registry::SessionRegistry;
use crate::agent_tab::profile::{AgentKind, AgentKindExt as _};

mod branch;
mod children;
mod events;
mod history;
pub(in crate::agent_tab) mod inbox;
mod maintenance;
mod questions;
mod registry;
mod startup;
#[cfg(test)]
mod tests;
mod workflows;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(Uuid);

pub struct AgentSession {
    pub(in crate::agent_tab) child_refresh: Option<Task<()>>,
    pub(in crate::agent_tab) child_readers: Rc<RefCell<HashMap<(u64, BackgroundTaskKey), usize>>>,
    pub(in crate::agent_tab) workflow_refresh: Option<Task<()>>,
    pub(in crate::agent_tab) workflow_readers: Rc<Cell<usize>>,
    pub(in crate::agent_tab) controller: Rc<RefCell<SessionController>>,
    pub(in crate::agent_tab) profile: AgentProfile,
    pub(in crate::agent_tab) workspace: AgentWorkspace,
    pub(in crate::agent_tab) active_workspace: AgentWorkspace,
    pub(in crate::agent_tab) route: AgentRoute,
    pub(in crate::agent_tab) kind: AgentKind,
    id: SessionId,
    pub(in crate::agent_tab) last_completed: Option<(u64, u64)>,
    closed: Rc<Cell<bool>>,
    binding_generation: Rc<Cell<u64>>,
    pub(in crate::agent_tab) team_launch: Option<TeamLaunch>,
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

pub(in crate::agent_tab) struct CommandBinding {
    closed: Rc<Cell<bool>>,
    current: Rc<Cell<u64>>,
    pub(in crate::agent_tab) generation: u64,
}

impl CommandBinding {
    pub(in crate::agent_tab) fn is_current(&self) -> bool {
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

pub(in crate::agent_tab) struct PresentationEffect {
    pub(in crate::agent_tab) epoch: u64,
    pub(in crate::agent_tab) generation: u64,
    pub(in crate::agent_tab) effect: RefCell<Option<SessionEffect>>,
}

impl EventEmitter<PresentationEffect> for AgentSession {}

impl EventEmitter<AgentPaneEvent> for AgentSession {}

#[derive(Clone)]
pub(in crate::agent_tab) enum ExecutionSignal {
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

    pub(in crate::agent_tab) fn bind(&self) -> CommandBinding {
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
    pub fn create(profile: AgentProfile, workspace: AgentWorkspace, cx: &mut App) -> SessionOwner {
        Self::create_with_team(profile, workspace, None, cx)
    }

    pub(in crate::agent_tab) fn create_team(
        profile: AgentProfile,
        workspace: AgentWorkspace,
        policy: TeamLaunch,
        cx: &mut App,
    ) -> SessionOwner {
        Self::create_with_team(profile, workspace, Some(policy), cx)
    }

    fn create_with_team(
        profile: AgentProfile,
        workspace: AgentWorkspace,
        team_launch: Option<TeamLaunch>,
        cx: &mut App,
    ) -> SessionOwner {
        let kind = AgentKind::from_profile(profile.kind);
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
            workflow_refresh: None,
            child_refresh: None,
            child_readers: Rc::new(RefCell::new(HashMap::new())),
            workflow_readers: Rc::new(Cell::new(0)),
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

    pub(in crate::agent_tab) fn publish(&self, effect: SessionEffect, cx: &mut Context<Self>) {
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
}
