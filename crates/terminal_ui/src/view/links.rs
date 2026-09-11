use gpui::{Context, Modifiers, ModifiersChangedEvent, Pixels, Point, Window};

use crate::view::TerminalPane;

/// Whether a click held down the modifier that follows a link.
///
/// macOS reserves Control-click for the secondary click, so a Control-click on
/// a link would open a context menu at the same time; there the modifier is
/// Command, which is also what its browsers and editors follow links on.
pub(crate) fn follows_link(modifiers: Modifiers) -> bool {
    #[cfg(target_os = "macos")]
    let modifier_held = modifiers.platform && !modifiers.control;

    #[cfg(not(target_os = "macos"))]
    let modifier_held = modifiers.control && !modifiers.platform;

    modifier_held && !modifiers.alt && !modifiers.shift
}

impl TerminalPane {
    pub(super) fn update_hovered_link(
        &mut self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let inside = self
            .content_bounds
            .is_some_and(|bounds| bounds.contains(&position));
        let local = self.local_position(position);
        self.model.links.enabled = inside && follows_link(modifiers);
        let hit = (inside && follows_link(modifiers))
            .then(|| self.model.link_at_position(local))
            .flatten();
        if self.model.links.update(local, hit) {
            cx.notify();
        }
    }

    pub(super) fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model.links.enabled = follows_link(event.modifiers);
        if let Some(position) = self.model.links.position() {
            let hit = follows_link(event.modifiers)
                .then(|| self.model.link_at_position(position))
                .flatten();
            if self.model.links.update(position, hit) {
                cx.notify();
            }
        }
    }
}
