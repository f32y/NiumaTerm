#[cfg(test)]
mod tests;

use gpui::prelude::*;
use gpui::{Context, Entity, Global, Render, WeakEntity, Window, div};
use gpui_base::{TextViewDefaults, Theme as BaseTheme};
use gpui_component::Theme;

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::AgentSettings;

/// The composer uses the pane's layout tree on every frame. Switching to an
/// independently laid out cached root changes pixel rounding at fractional
/// pane offsets, moving the controls and caret even when their content is unchanged.
pub(crate) struct ComposerView {
    owner: WeakEntity<AgentPane>,
}

impl ComposerView {
    pub(crate) fn new(owner: Entity<AgentPane>, cx: &mut Context<Self>) -> Self {
        cx.observe(&owner, |_, _, cx| cx.notify()).detach();

        observe_style::<AgentSettings>(cx);
        observe_style::<Theme>(cx);
        observe_style::<BaseTheme>(cx);
        observe_style::<TextViewDefaults>(cx);

        Self {
            owner: owner.downgrade(),
        }
    }
}

impl Render for ComposerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.owner
            .update(cx, |pane, cx| pane.render_composer(window, cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}

fn observe_style<G: Global>(cx: &mut Context<ComposerView>) {
    cx.observe_global::<G>(|_, cx| cx.notify()).detach();
}
