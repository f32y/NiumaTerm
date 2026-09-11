use nmt_terminal::session::HostEvent;

use crate::view::TerminalPane;

impl TerminalPane {
    pub fn drain_host_events(&mut self) -> Vec<HostEvent> {
        self.model.drain_host_events()
    }
}
