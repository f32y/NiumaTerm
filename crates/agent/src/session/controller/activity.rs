use crate::background_task::{BackgroundTaskKey, BackgroundTaskSnapshot};
use crate::session::children::{ChildAgents, ChildTranscript, scoped_background_tasks};
use crate::session::controller::SessionController;
use crate::session::update_readiness::{ConversationWork, Readiness, prepare_stop};

impl SessionController {
    pub fn background_task_parent(&self) -> Option<BackgroundTaskKey> {
        ChildAgents::parent(self.runtime.backend()?.recovery_identity()?)
    }

    pub fn background_tasks(&self) -> Option<&BackgroundTaskSnapshot> {
        scoped_background_tasks(
            self.background_task_parent().as_ref(),
            self.children.background_tasks.as_ref(),
        )
    }

    pub fn background_task_transcript(&self, key: &BackgroundTaskKey) -> Option<&ChildTranscript> {
        self.background_tasks()?;

        self.children.transcripts.get(key)
    }

    pub fn background_activity(&self) -> (usize, usize) {
        self.background_tasks()
            .map_or((0, 0), |tasks| (tasks.tasks.len(), tasks.active_count()))
    }

    pub(super) fn set_background_tasks(&mut self, snapshot: BackgroundTaskSnapshot) -> bool {
        let before = self.background_activity();

        self.children.background_tasks = Some(snapshot);

        self.background_activity() != before
    }

    pub fn refresh_background_tasks(&mut self) {
        if let Some(backend) = self.runtime.backend_mut() {
            backend.refresh_background_tasks();
        }
    }

    pub fn interrupt_background_task(&mut self, key: &BackgroundTaskKey) -> bool {
        self.runtime
            .backend_mut()
            .is_some_and(|backend| backend.interrupt_background_task(key))
    }

    pub fn update_readiness(&self, work: ConversationWork) -> Readiness {
        work.readiness(&self.runtime, &self.commands, &self.delivery)
    }

    pub fn prepare_update_stop(&mut self) {
        prepare_stop(&mut self.commands, &mut self.delivery);
    }
}
