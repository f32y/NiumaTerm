use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, Global, Pixels, Render, StyleRefinement, TextStyle,
    WeakEntity, Window, div,
};
use gpui_component::ElementExt as _;

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::AgentSettings;

pub(crate) struct ComposerView {
    owner: WeakEntity<AgentPane>,
    height: Rc<Cell<Option<Pixels>>>,
    style: Option<(TextStyle, Pixels, f32)>,
    width: Option<Pixels>,
    #[cfg(test)]
    pub(crate) renders: usize,
}

impl ComposerView {
    pub(crate) fn new(owner: Entity<AgentPane>, cx: &mut Context<Self>) -> Self {
        cx.observe(&owner, |view, _, cx| {
            view.height.set(None);

            cx.notify();
        })
        .detach();

        observe_style::<AgentSettings>(cx);
        observe_style::<gpui_component::Theme>(cx);
        observe_style::<gpui_base::Theme>(cx);
        observe_style::<gpui_base::TextViewDefaults>(cx);

        Self {
            owner: owner.downgrade(),
            height: Rc::default(),
            style: None,
            width: None,
            #[cfg(test)]
            renders: 0,
        }
    }

    pub(crate) fn element(view: &Entity<Self>, window: &Window, cx: &mut App) -> AnyElement {
        let height = view.update(cx, |view, cx| {
            let style = (
                window.text_style(),
                window.rem_size(),
                window.scale_factor(),
            );

            if window.is_view_dirty(cx.entity_id()) || view.style.as_ref() != Some(&style) {
                view.height.set(None);

                view.style = Some(style);
            }

            view.height.get()
        });

        let content = match height {
            Some(height) => view
                .clone()
                .cached(StyleRefinement::default().w_full().h(height))
                .into_any_element(),
            None => view.clone().into_any_element(),
        };

        let view = view.downgrade();

        div()
            .w_full()
            .on_prepaint(move |bounds, _, cx| {
                let _ = view.update(cx, |view, cx| {
                    if view.width != Some(bounds.size.width) {
                        view.width = Some(bounds.size.width);

                        view.height.set(None);

                        cx.notify();
                    }
                });
            })
            .child(content)
            .into_any_element()
    }
}

impl Render for ComposerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }

        let body = self
            .owner
            .update(cx, |pane, cx| pane.render_composer(window, cx))
            .unwrap_or_else(|_| div().into_any_element());

        let height = self.height.clone();

        div().w_full().child(body).on_prepaint(move |bounds, _, _| {
            height.set(Some(bounds.size.height));
        })
    }
}

fn observe_style<G: Global>(cx: &mut Context<ComposerView>) {
    cx.observe_global::<G>(|view, cx| {
        view.height.set(None);

        cx.notify();
    })
    .detach();
}
