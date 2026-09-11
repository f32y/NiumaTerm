//! Session execution and logical lifetime, independent of presentation entities.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use std::{fs, thread};

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Task, WeakEntity};
use nmt_agent::background_task::BackgroundTaskKey;
use nmt_agent::session::RecoveryIdentity;
use nmt_agent::session::controller::{SessionController, SessionEffect};
use nmt_agent::{AgentRoute, AgentWorkspace, agent_process};
use nmt_config::profile::AgentProfile;
use uuid::Uuid;

use crate::AgentPaneEvent;
use crate::composer::attachments::scratch_dir;
pub use crate::execution::children::ChildReader;
pub use crate::execution::registry::SessionRegistry;
use crate::profile::{AgentKind, AgentKindExt as _};

mod branch;
mod children;
mod events;
mod history;
pub(crate) mod inbox;
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
    pub(crate) child_refresh: Option<Task<()>>,
    pub(crate) child_readers: Rc<RefCell<HashMap<(u64, BackgroundTaskKey), usize>>>,
    pub(crate) workflow_refresh: Option<Task<()>>,
    pub(crate) workflow_readers: Rc<Cell<usize>>,
    pub(crate) controller: Rc<RefCell<SessionController>>,
    pub(crate) profile: AgentProfile,
    pub(crate) workspace: AgentWorkspace,
    pub(crate) active_workspace: AgentWorkspace,
    pub(crate) route: AgentRoute,
    pub(crate) kind: AgentKind,
    id: SessionId,
    pub(crate) last_completed: Option<(u64, u64)>,
    closed: Rc<Cell<bool>>,
    binding_generation: Rc<Cell<u64>>,
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

pub(crate) struct CommandBinding {
    closed: Rc<Cell<bool>>,
    current: Rc<Cell<u64>>,
    pub(crate) generation: u64,
}

impl CommandBinding {
    pub(crate) fn is_current(&self) -> bool {
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

pub(crate) struct PresentationEffect {
    pub(crate) epoch: u64,
    pub(crate) generation: u64,
    pub(crate) effect: RefCell<Option<SessionEffect>>,
}

impl EventEmitter<PresentationEffect> for AgentSession {}

impl EventEmitter<AgentPaneEvent> for AgentSession {}

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

    pub(crate) fn bind(&self) -> CommandBinding {
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

    pub(crate) fn publish(&self, effect: SessionEffect, cx: &mut Context<Self>) {
        cx.emit(PresentationEffect {
            epoch: self.controller.borrow().runtime.epoch(),
            generation: self.binding_generation.get(),
            effect: RefCell::new(Some(effect)),
        });

        cx.notify();
    }
}
