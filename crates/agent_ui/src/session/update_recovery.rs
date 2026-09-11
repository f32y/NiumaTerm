use gpui::{App, Context};

use crate::AgentPane;
use crate::session::RecoverySnapshot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryReadiness {
    Ready(RecoverySnapshot),
    Busy(String),
    MissingIdentity(String),
}

impl AgentPane {
    pub fn recovery_readiness(&self, cx: &App) -> RecoveryReadiness {
        self.host.upgrade().map_or_else(
            || RecoveryReadiness::Busy("session closed".into()),
            |host| host.read(cx).recovery_readiness(cx),
        )
    }

    pub(crate) fn retry_update_recovery(&mut self, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.retry_update_recovery(cx));
        }
    }

    pub(crate) fn start_new_after_update_failure(&mut self, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.start_new_after_update_failure(cx));
        }
    }
}
