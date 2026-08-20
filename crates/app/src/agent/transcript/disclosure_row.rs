use crate::agent::transcript::view::TranscriptView;
use crate::agent::*;

pub(super) const AGENT_DISCLOSURE_SLOT: f32 = 16.0;
pub(super) const AGENT_DISCLOSURE_GAP: f32 = 4.0;
pub(super) const AGENT_DISCLOSURE_PADDING: f32 = 4.0;
pub(super) const AGENT_DISCLOSURE_DETAIL_INSET: f32 =
    AGENT_DISCLOSURE_PADDING + AGENT_DISCLOSURE_SLOT * 2.0 + AGENT_DISCLOSURE_GAP * 2.0;
/// Monospaced glyphs average roughly 0.6em wide, so a rem measure is about
/// 1.67 characters. 48rem gives assistant output an 80-character line.
pub(super) const AGENT_TEXT_MEASURE_REMS: f32 = 48.0;

/// A prompt is the user's own words read back, and it sits in a tinted bubble
/// against the right edge. A shorter line keeps that block from spanning the
/// pane and reads as an aside to the reply beside it: 30rem is 50 characters.
pub(super) const USER_TEXT_MEASURE_REMS: f32 = 30.0;

/// Shared geometry for expandable transcript rows. Empty chevron, type-icon,
/// and trailing slots keep labels aligned by default; summary toggles can omit
/// the unused type-icon slot so their label follows the chevron directly.
pub(in crate::agent) struct AgentDisclosureRow {
    id: ElementId,
    expanded: Option<bool>,
    type_icon: Option<IconName>,
    reserve_type_icon_slot: bool,
    label: String,
    preview: Option<String>,
    trailing: Option<AnyElement>,
    accessible_label: String,
    /// Tint for the label and type icon, replacing the quiet work-log default.
    /// The row sets its own text colors per slot, so a caller cannot override
    /// them from the outside.
    accent: Option<Hsla>,
}

impl AgentDisclosureRow {
    pub(super) fn new(id: impl Into<ElementId>, label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            id: id.into(),
            expanded: None,
            type_icon: None,
            reserve_type_icon_slot: true,
            accessible_label: label.clone(),
            label,
            preview: None,
            trailing: None,
            accent: None,
        }
    }

    /// Mark the row as a structural break rather than one step of the work log.
    pub(super) fn accent(mut self, color: Hsla) -> Self {
        self.accent = Some(color);
        self
    }

    pub(super) fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
        self
    }

    pub(super) fn type_icon(mut self, icon: IconName) -> Self {
        self.type_icon = Some(icon);
        self
    }

    pub(super) fn without_type_icon_slot(mut self) -> Self {
        self.reserve_type_icon_slot = false;
        self
    }

    pub(super) fn preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }

    pub(super) fn trailing(mut self, trailing: Option<AnyElement>) -> Self {
        self.trailing = trailing;
        self
    }

    pub(super) fn accessible_label(mut self, label: impl Into<String>) -> Self {
        self.accessible_label = label.into();
        self
    }

    pub(super) fn render(self, cx: &mut Context<TranscriptView>) -> Stateful<Div> {
        let hover_bg = cx.theme().muted.opacity(0.4);
        let expandable = self.expanded.is_some();
        let chevron = self.expanded.map(|expanded| {
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size_3()
            .text_color(cx.theme().muted_foreground.opacity(0.7))
        });
        let show_type_icon_slot = self.type_icon.is_some() || self.reserve_type_icon_slot;
        let icon_color = self
            .accent
            .unwrap_or_else(|| cx.theme().muted_foreground.opacity(0.8));
        let label_color = self
            .accent
            .unwrap_or_else(|| cx.theme().foreground.opacity(0.82));
        let type_icon = self
            .type_icon
            .map(|icon| Icon::new(icon).size_3p5().text_color(icon_color));
        h_flex()
            .id(self.id)
            .w_full()
            // The row's hover fill would otherwise run to the pane's edge,
            // marking a band far wider than the text column it belongs to.
            // Holding it to the reading measure keeps its right edge with the
            // assistant prose above it, and a preview still truncates there.
            .max_w(rems(AGENT_TEXT_MEASURE_REMS))
            .min_h(px(24.))
            .gap(px(AGENT_DISCLOSURE_GAP))
            .items_center()
            .px(px(AGENT_DISCLOSURE_PADDING))
            .py_0p5()
            .rounded(UI_RADIUS)
            .aria_label(self.accessible_label)
            .when(expandable, |this| {
                this.cursor_pointer()
                    .role(gpui::Role::Button)
                    .hover(move |style| style.bg(hover_bg))
            })
            .child(
                div()
                    .w(px(AGENT_DISCLOSURE_SLOT))
                    .h(px(AGENT_DISCLOSURE_SLOT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(chevron),
            )
            .when(show_type_icon_slot, |this| {
                this.child(
                    div()
                        .w(px(AGENT_DISCLOSURE_SLOT))
                        .h(px(AGENT_DISCLOSURE_SLOT))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .children(type_icon),
                )
            })
            .child(
                div()
                    .flex_none()
                    .max_w(relative(0.6))
                    .truncate()
                    .text_sm()
                    .text_color(label_color)
                    .child(self.label),
            )
            .children(self.preview.map(|preview| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground.opacity(0.55))
                    .child(preview)
            }))
            .child(
                div()
                    .w(px(AGENT_DISCLOSURE_SLOT))
                    .h(px(AGENT_DISCLOSURE_SLOT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(self.trailing),
            )
    }
}
