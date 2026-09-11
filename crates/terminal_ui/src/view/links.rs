use gpui::{Context, ModifiersChangedEvent, Window};

use crate::view::TerminalPane;
use crate::view::key::modifiers_state;

impl TerminalPane {
    pub(super) fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .model
            .hover_modifiers_changed(modifiers_state(event.modifiers))
        {
            cx.notify();
        }
    }
}
