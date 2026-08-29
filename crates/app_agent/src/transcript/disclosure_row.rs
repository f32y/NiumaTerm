use crate::transcript::view::TranscriptView;
use crate::*;

/// Card geometry for the transcript's expandable rows. Every tool call, work
/// run and structural break in the conversation is drawn as one card, so these
/// are the numbers that keep their headers on a single baseline grid.
pub(super) const AGENT_CARD_RADIUS: f32 = 12.0;
pub(super) const AGENT_CARD_PADDING_X: f32 = 14.0;
pub(super) const AGENT_CARD_PADDING_Y: f32 = 10.0;
pub(super) const AGENT_CARD_GAP: f32 = 10.0;
/// The square that holds a row's type icon, and the icon drawn inside it.
pub(super) const AGENT_CARD_ICON_BLOCK: f32 = 26.0;
pub(super) const AGENT_CARD_ICON_BLOCK_RADIUS: f32 = 7.0;
pub(super) const AGENT_CARD_ICON: f32 = 13.0;
/// Where a card's title starts, measured from the card's leading edge. The
/// expanded body lines up with the title rather than with the icon, so a run
/// of detail lines reads as belonging to the heading above it.
pub(super) const AGENT_DISCLOSURE_DETAIL_INSET: f32 =
    AGENT_CARD_PADDING_X + AGENT_CARD_ICON_BLOCK + AGENT_CARD_GAP;
/// Text sizes inside a card: the title, and the quieter runs beside it.
pub(super) const AGENT_CARD_TITLE_SIZE: f32 = 13.0;
pub(super) const AGENT_CARD_DETAIL_SIZE: f32 = 12.0;
pub(super) const AGENT_CARD_HINT_SIZE: f32 = 11.0;
/// A prompt is the user's own words read back, and it sits in a tinted bubble
/// against the right edge. Capping it at a share of the transcript column keeps
/// that block from spanning the pane, so it reads as an aside to the reply
/// beside it while still growing with the window.
pub(super) const USER_BUBBLE_WIDTH_FRACTION: f32 = 0.7;
/// The prompt bubble's corners. The trailing bottom corner is nearly square so
/// the bubble points back at the conversation it was sent into, which is what
/// separates it from the assistant's bare prose at a glance.
pub(super) const USER_BUBBLE_RADIUS: f32 = 18.0;
pub(super) const USER_BUBBLE_TAIL_RADIUS: f32 = 4.0;
pub(super) const USER_BUBBLE_PADDING_X: f32 = 15.0;
pub(super) const USER_BUBBLE_PADDING_Y: f32 = 9.0;

/// How a card is tinted. A failed step is the one thing in a conversation the
/// reader has to act on, so it carries the tint across the whole card rather
/// than on a glyph that has to be found first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentCardTone {
    Neutral,
    Failed,
}

/// Border, fill and icon-block colors for one tone.
pub(super) struct AgentCardColors {
    pub(super) border: Hsla,
    pub(super) background: Hsla,
    pub(super) icon_block: Hsla,
    pub(super) icon: Hsla,
    pub(super) hover: Hsla,
}

impl AgentCardTone {
    pub(super) fn colors(self, cx: &App) -> AgentCardColors {
        match self {
            Self::Neutral => AgentCardColors {
                border: cx.theme().border,
                background: cx.theme().popover,
                icon_block: cx.theme().muted,
                icon: cx.theme().muted_foreground,
                hover: cx.theme().muted.opacity(0.4),
            },
            Self::Failed => AgentCardColors {
                border: cx.theme().danger.opacity(0.3),
                background: cx.theme().danger.opacity(0.05),
                icon_block: cx.theme().danger.opacity(0.1),
                icon: cx.theme().danger,
                hover: cx.theme().danger.opacity(0.09),
            },
        }
    }
}

/// The card a disclosure row and its expanded body share. Returned empty so
/// the caller can hang the header, the failure reason and the detail surface
/// off one container without each of them re-deriving the tone.
pub(super) fn agent_card(tone: AgentCardTone, cx: &App) -> Div {
    let colors = tone.colors(cx);

    v_flex()
        .w_full()
        .overflow_hidden()
        .rounded(px(AGENT_CARD_RADIUS))
        .border_1()
        .border_color(colors.border)
        .bg(colors.background)
}

/// One status pill: the step's own outcome, stated rather than drawn as a
/// glyph the reader has to decode.
pub(super) fn agent_card_badge(label: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_none()
        .rounded(px(6.))
        .px(px(8.))
        .py(px(2.))
        .text_size(px(AGENT_CARD_DETAIL_SIZE))
        .bg(color.opacity(0.1))
        .text_color(color)
        .child(label.into())
}

/// Shared header for the transcript's expandable rows: icon block, title, and
/// the quieter runs that qualify it. A row without an expanded state renders
/// the same header without the disclosure hint, so a plain step and an
/// expandable one keep one shape.
pub(crate) struct AgentDisclosureRow {
    id: ElementId,
    expanded: Option<bool>,
    type_icon: Option<IconName>,
    label: String,
    /// The technical text the heading names — a command line, a path — set in
    /// the transcript's code font beside the title.
    mono_detail: Option<String>,
    preview: Option<String>,
    badge: Option<(String, Hsla)>,
    accessible_label: String,
    tone: AgentCardTone,
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
            accessible_label: label.clone(),
            label,
            mono_detail: None,
            preview: None,
            badge: None,
            tone: AgentCardTone::Neutral,
            accent: None,
        }
    }

    /// Mark the row as a structural break rather than one step of the work log.
    pub(super) fn accent(mut self, color: Hsla) -> Self {
        self.accent = Some(color);
        self
    }

    pub(super) fn tone(mut self, tone: AgentCardTone) -> Self {
        self.tone = tone;
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

    pub(super) fn mono_detail(mut self, detail: impl Into<String>) -> Self {
        self.mono_detail = Some(detail.into());
        self
    }

    pub(super) fn preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }

    pub(super) fn badge(mut self, label: impl Into<String>, color: Hsla) -> Self {
        self.badge = Some((label.into(), color));
        self
    }

    pub(super) fn accessible_label(mut self, label: impl Into<String>) -> Self {
        self.accessible_label = label.into();
        self
    }

    pub(super) fn render(self, cx: &mut Context<TranscriptView>) -> Stateful<Div> {
        let colors = self.tone.colors(cx);
        let expandable = self.expanded.is_some();
        let hint = self.expanded.map(|expanded| {
            if expanded {
                i18n("agent-transcript-collapse-hint")
            } else {
                i18n("agent-transcript-expand-hint")
            }
        });
        let icon_color = self.accent.unwrap_or(colors.icon);
        let label_color = self
            .accent
            .unwrap_or_else(|| cx.theme().foreground.opacity(0.9));
        let type_icon = self.type_icon.map(|icon| {
            Icon::new(icon)
                .size(px(AGENT_CARD_ICON))
                .text_color(icon_color)
        });
        let mono_font = cx.global::<AgentSettings>().transcript_font();

        h_flex()
            .id(self.id)
            .w_full()
            .gap(px(AGENT_CARD_GAP))
            .items_center()
            .px(px(AGENT_CARD_PADDING_X))
            .py(px(AGENT_CARD_PADDING_Y))
            .aria_label(self.accessible_label)
            .when(expandable, |this| {
                this.cursor_pointer()
                    .role(gpui::Role::Button)
                    .hover(move |style| style.bg(colors.hover))
            })
            .child(
                div()
                    .size(px(AGENT_CARD_ICON_BLOCK))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(AGENT_CARD_ICON_BLOCK_RADIUS))
                    .bg(colors.icon_block)
                    .children(type_icon),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(relative(0.5))
                    .truncate()
                    .text_size(px(AGENT_CARD_TITLE_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(label_color)
                    .child(self.label),
            )
            .children(self.mono_detail.map(|detail| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font(mono_font)
                    .text_size(px(AGENT_CARD_DETAIL_SIZE))
                    .text_color(cx.theme().muted_foreground)
                    .child(detail)
            }))
            .children(self.preview.map(|preview| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(AGENT_CARD_DETAIL_SIZE))
                    .text_color(cx.theme().muted_foreground.opacity(0.8))
                    .child(preview)
            }))
            // Whatever the row had to say about itself has taken its width by
            // now; this spacer closes the row from the trailing edge whether
            // or not those middle slots were filled.
            .child(div().flex_1().min_w_0())
            .children(
                self.badge
                    .map(|(label, color)| agent_card_badge(label, color)),
            )
            .children(hint.map(|hint| {
                div()
                    .flex_none()
                    .text_size(px(AGENT_CARD_HINT_SIZE))
                    .text_color(cx.theme().muted_foreground.opacity(0.8))
                    .child(hint)
            }))
    }
}
