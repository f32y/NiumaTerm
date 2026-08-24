use gpui::prelude::*;
use gpui::{
    App, ElementId, Entity, IntoElement, KeyDownEvent, MouseButton, RenderOnce, SharedString,
    Window, div,
};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum InlineRenameStyle {
    HorizontalTab,
    SidebarTab,
    Workspace,
}

/// Presentation for a Shell-owned rename input. Enter and blur are handled by
/// the input subscription; this element only protects row activation and sends
/// Escape to the caller's cancellation path.
#[derive(IntoElement)]
pub(in crate::ui) struct InlineRename {
    id: ElementId,
    label: SharedString,
    input: Entity<InputState>,
    style: InlineRenameStyle,
    cancel: Box<dyn Fn(&mut Window, &mut App)>,
}

impl InlineRename {
    pub(in crate::ui) fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        input: Entity<InputState>,
        style: InlineRenameStyle,
        cancel: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            input,
            style,
            cancel: Box::new(cancel),
        }
    }
}

impl RenderOnce for InlineRename {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let container = div()
            .id(self.id)
            .aria_label(self.label)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let container = match self.style {
            InlineRenameStyle::HorizontalTab => container.flex_1(),
            InlineRenameStyle::SidebarTab => container.flex_1().overflow_hidden(),
            InlineRenameStyle::Workspace => container.w_full().text_left().text_sm().truncate(),
        };
        let input = match self.style {
            InlineRenameStyle::HorizontalTab => Input::new(&self.input)
                .small()
                .p_0()
                .text_center()
                .appearance(false),
            InlineRenameStyle::SidebarTab => Input::new(&self.input)
                .xsmall()
                .p_0()
                .text_xs()
                .appearance(false),
            InlineRenameStyle::Workspace => Input::new(&self.input)
                .xsmall()
                .p_0()
                .text_sm()
                .appearance(false),
        };
        let cancel = self.cancel;

        container
            .capture_key_down(move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cancel(window, cx);
                }
            })
            .child(input)
    }
}
