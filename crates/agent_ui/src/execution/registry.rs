use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{App, Entity, Global, WeakEntity};

use crate::execution::{AgentSession, SessionId};

#[derive(Default)]
pub struct SessionRegistry(pub(crate) Rc<RefCell<HashMap<SessionId, WeakEntity<AgentSession>>>>);

impl Global for SessionRegistry {}

impl SessionRegistry {
    pub fn sessions(cx: &App) -> Vec<Entity<AgentSession>> {
        cx.try_global::<Self>()
            .map(|registry| {
                registry
                    .0
                    .borrow()
                    .values()
                    .filter_map(WeakEntity::upgrade)
                    .filter(|session| !session.read(cx).is_closed())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get(id: SessionId, cx: &App) -> Option<Entity<AgentSession>> {
        let session = cx.try_global::<Self>()?.0.borrow().get(&id)?.upgrade()?;

        (!session.read(cx).is_closed()).then_some(session)
    }
}
