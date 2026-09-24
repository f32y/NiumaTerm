//! Small shared building blocks of the window chrome: sizes, surface styles,
//! toolbar controls, empty states, hover actions, progress edges, Git colors
//! and status marks.

use std::time::Duration;

use app::design::SURFACE_RADIUS;
use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Div, ElementId, Hsla, IntoElement, Pixels,
    RenderOnce, SharedString, Stateful, StyleRefinement, Window, div, ease_in_out, px, relative,
    rgb,
};
use gpui_component::button::{Button, ButtonVariants as _, Toggle, ToggleVariants as _};
use gpui_component::progress::ProgressCircle;
use gpui_component::{ActiveTheme, Sizable as _, v_flex};

use crate::ui::UI_RADIUS;

/// Bottom gutter for the workspace sidebar, which floats clear of the window
/// edge; the main pane runs into that edge instead.
pub(crate) const FLOATING_SURFACE_BOTTOM_INSET: f32 = 6.0;

/// Toolbar controls fit within the title bar without stretching its height.
pub(crate) const TOOLBAR_BUTTON_SIZE: f32 = 30.0;

/// Edge of the icon a toolbar button centers in itself. The button's pixel
/// size is a style override, so its icon keeps the component's medium
/// 16px size. The sidebar sets its whole content column on the edge of this
/// icon box, which is what lines the section heading, the tab glyphs and
/// the status rows up under the app menu button.
pub(crate) const TOOLBAR_ICON_SIZE: f32 = 16.0;

#[derive(Clone, Copy)]
pub(crate) struct SidebarSelection {
    pub(crate) active_background: Hsla,
    pub(crate) active_foreground: Hsla,
    pub(crate) idle_foreground: Hsla,
    pub(crate) hover_background: Hsla,
}

/// Workspace buttons and vertical tab rows use the same selection language
/// even though their component types require different style application.
pub(crate) fn sidebar_selection(cx: &App) -> SidebarSelection {
    SidebarSelection {
        active_background: cx.theme().sidebar_accent,
        active_foreground: cx.theme().sidebar_accent_foreground,
        idle_foreground: cx.theme().sidebar_foreground.opacity(0.75),
        hover_background: cx.theme().sidebar_accent.opacity(0.4),
    }
}

/// A full bordered region whose children must stay clipped to rounded edges.
pub(crate) fn framed_region(cx: &App) -> StyleRefinement {
    StyleRefinement::default()
        .w_full()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(UI_RADIUS)
        .overflow_hidden()
}

/// Shared title strip for the interchangeable right-side panel contents.
pub(crate) fn panel_header(cx: &App) -> StyleRefinement {
    StyleRefinement::default()
        .px_2()
        .py_1()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().sidebar_border)
}

/// Sidebar-content surface shared by the right panel and Settings navigation.
/// Callers retain ownership of size and any edge they intentionally suppress.
pub(crate) fn sidebar_surface(cx: &App) -> StyleRefinement {
    StyleRefinement::default()
        .border_1()
        .border_color(cx.theme().sidebar_border)
        .rounded(UI_RADIUS)
        .overflow_hidden()
        .bg(cx.theme().sidebar)
}

/// Common table heading treatment. Callers retain ownership of height and text
/// size because those measurements differ between settings and usage tables.
pub(crate) fn table_header(cx: &App) -> StyleRefinement {
    StyleRefinement::default()
        .w_full()
        .px_3()
        .gap_2()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted.opacity(0.4))
        .text_color(cx.theme().muted_foreground)
}

/// Standalone icon controls keep the same visible and clickable square.
pub(crate) fn toolbar_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).ghost().size(px(TOOLBAR_BUTTON_SIZE)).p_0()
}

/// A toggle keeps a square icon target and can grow for an optional count.
pub(crate) fn toolbar_toggle(id: impl Into<ElementId>) -> Toggle {
    Toggle::new(id)
        .ghost()
        .min_w(px(TOOLBAR_BUTTON_SIZE))
        .h(px(TOOLBAR_BUTTON_SIZE))
        .px(px(6.))
}

pub(crate) fn empty_state(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    cx: &App,
) -> AnyElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_1()
        .px(px(16.0))
        .child(div().text_sm().child(title.into()))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail.into()),
        )
        .into_any_element()
}

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

/// Progress track along the bottom edge of a rounded row, filled to
/// `fraction`. One corner radius of space at each side keeps the track on the
/// straight part of the edge. The row must be `relative`, since the track is
/// placed out of the row's flow.
pub(crate) fn progress_edge(fraction: f32, color: Hsla) -> Div {
    div()
        .absolute()
        .bottom_0()
        .left(SURFACE_RADIUS)
        .right(SURFACE_RADIUS)
        .h(px(2.0))
        .child(
            div()
                .h_full()
                .w(relative(fraction))
                .rounded_full()
                .bg(color),
        )
}

pub(crate) struct GitColors {
    pub(crate) added: Hsla,
    pub(crate) removed: Hsla,
    pub(crate) added_background: Hsla,
    pub(crate) removed_background: Hsla,
    pub(crate) added_word: Hsla,
    pub(crate) removed_word: Hsla,
}

impl GitColors {
    pub(crate) fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let dark = theme.mode.is_dark();

        let added = theme.green;
        let removed = theme.red;

        // Review fills have their own semantic palette: terminal ANSI colors
        // vary too widely to produce a consistent row contrast by blending.
        let (added_background, removed_background, added_word, removed_word) = if dark {
            (0x243d2d, 0x482c30, 0x355c40, 0x713e46)
        } else {
            (0xe1fce1, 0xfee4e3, 0xb3e8b3, 0xf5b9b7)
        };

        Self {
            added,
            removed,
            added_background: rgb(added_background).into(),
            removed_background: rgb(removed_background).into(),
            added_word: rgb(added_word).into(),
            removed_word: rgb(removed_word).into(),
        }
    }
}

/// One breath of a pulsing mark, and how far its opacity travels. Slow enough
/// to read as ongoing work rather than as a blinking alert.
const PULSE_PERIOD: Duration = Duration::from_millis(1_600);

const PULSE_MIN_OPACITY: f32 = 0.35;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusMarkTone {
    Warning,
    Success,
}

enum StatusMarkVisual {
    Dot { tone: StatusMarkTone, size: Pixels },
    Busy,
}

/// A fixed-size semantic status dot. The caller owns state precedence and may
/// attach wording when the surrounding control does not already name the mark.
#[derive(IntoElement)]
pub(crate) struct StatusMark {
    id: ElementId,
    visual: StatusMarkVisual,
    label: Option<SharedString>,
    pulse: bool,
}

impl StatusMark {
    pub(crate) fn new(id: impl Into<ElementId>, tone: StatusMarkTone, size: Pixels) -> Self {
        Self {
            id: id.into(),
            visual: StatusMarkVisual::Dot { tone, size },
            label: None,
            pulse: false,
        }
    }

    pub(crate) fn busy(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            visual: StatusMarkVisual::Busy,
            label: None,
            pulse: false,
        }
    }

    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());

        self
    }

    /// Breathe the mark, for a dot that reports work still in flight. A dot
    /// says what the state is; the pulse is what says it is still moving,
    /// where a row has no room for a spinner beside its label.
    pub(crate) fn pulse(mut self) -> Self {
        self.pulse = true;

        self
    }
}

impl RenderOnce for StatusMark {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        match self.visual {
            StatusMarkVisual::Dot { tone, size } => {
                let mark = div()
                    .id(self.id.clone())
                    .flex_none()
                    .size(size)
                    .rounded_full()
                    .when_some(self.label, |this, label| this.aria_label(label));

                let mark = match tone {
                    StatusMarkTone::Warning => mark.bg(cx.theme().warning),
                    StatusMarkTone::Success => mark.bg(cx.theme().success),
                };

                if !self.pulse {
                    return mark.into_any_element();
                }

                mark.with_animation(
                    self.id,
                    Animation::new(PULSE_PERIOD).repeat(),
                    |mark, delta| {
                        // One breath per period: the ramp turns at the
                        // halfway point rather than snapping back to full.
                        let phase = 1.0 - (delta * 2.0 - 1.0).abs();

                        mark.opacity(
                            PULSE_MIN_OPACITY + (1.0 - PULSE_MIN_OPACITY) * ease_in_out(phase),
                        )
                    },
                )
                .into_any_element()
            }
            StatusMarkVisual::Busy => ProgressCircle::new(self.id)
                .small()
                .loading(true)
                .color(cx.theme().warning)
                .into_any_element(),
        }
    }
}
