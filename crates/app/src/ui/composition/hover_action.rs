use gpui::prelude::*;
use gpui::{Div, ElementId, IntoElement, SharedString, Stateful, div};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoverActionLayout {
    Bare,
    Inline,
    Fill,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HoverActionVisibility {
    Always,
    OnGroupHover(SharedString),
}

/// A stable auxiliary target whose command remains attached by the owning
/// view. Layout and visibility are explicit so narrow tab pills can reuse the
/// glyph slot while ordinary rows retain their inline spacing.
pub(crate) fn hover_action(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    layout: HoverActionLayout,
    visibility: HoverActionVisibility,
    child: impl IntoElement,
) -> Stateful<Div> {
    let action = div().id(id).aria_label(label);
    let action = match layout {
        HoverActionLayout::Bare => action,
        HoverActionLayout::Inline => action.px_1(),
        HoverActionLayout::Fill => action
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center(),
    };
    let action = match visibility {
        HoverActionVisibility::Always => action,
        HoverActionVisibility::OnGroupHover(group) => {
            action.invisible().group_hover(group, |this| this.visible())
        }
    };

    action.child(child)
}
