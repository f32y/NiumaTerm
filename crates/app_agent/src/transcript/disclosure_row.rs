use crate::transcript::view::TranscriptView;
use crate::*;

/// Card geometry for the transcript's expandable rows. Every tool call, work
/// run and structural break in the conversation is drawn as one card, so these
/// are the numbers that keep their headers on a single baseline grid.
pub(super) const AGENT_CARD_RADIUS: f32 = 6.0;
pub(super) const AGENT_CARD_PADDING_X: f32 = 14.0;
pub(super) const AGENT_CARD_PADDING_Y: f32 = 4.0;
/// Resting height of a card header. A floor rather than a fixed height: the
/// row keeps one rhythm down a run of steps, and still grows for a label that
/// wraps at a narrow pane width.
pub(super) const AGENT_CARD_HEADER_HEIGHT: f32 = 32.0;
/// Vertical inset of a card's body. Looser than the header's, because the
/// header is one line of labels while the body is a block of output that
/// needs air between it and the rule above it.
pub(super) const AGENT_CARD_BODY_PADDING_Y: f32 = 8.0;
pub(super) const AGENT_CARD_GAP: f32 = 10.0;
/// The square that holds a row's type icon, and the icon drawn inside it.
pub(super) const AGENT_CARD_ICON_BLOCK: f32 = 18.0;
pub(super) const AGENT_CARD_ICON_BLOCK_RADIUS: f32 = 5.0;
pub(super) const AGENT_CARD_ICON: f32 = 11.0;
/// Leading inside a card header. The transcript sets prose leading for text
/// read in paragraphs; a header is one line of labels, and inheriting that
/// leading is what would otherwise decide the card's height.
pub(super) const AGENT_CARD_LINE_HEIGHT: f32 = 1.2;
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
            // Tinted with the theme's own foreground-alpha wash rather than
            // filled with the popover colour. A popover is opaque and near
            // white in most light themes, which over a grey panel reads as a
            // sheet of paper laid on the pane; the wash lifts the card off
            // whatever the pane paints without ever leaving that surface.
            Self::Neutral => AgentCardColors {
                border: cx.theme().border,
                background: cx.theme().accent,
                icon_block: cx.theme().muted,
                icon: cx.theme().muted_foreground,
                hover: cx.theme().list_hover,
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

/// Shared header for the transcript's expandable rows: icon block, title, and
/// the quieter runs that qualify it. A row without an expanded state renders
/// the same header without the disclosure hint, so a plain step and an
/// expandable one keep one shape.
pub(crate) struct AgentDisclosureRow {
    id: ElementId,
    expanded: Option<bool>,
    type_icon: Option<IconName>,
    label: String,
    preview: Option<String>,
    /// The step's outcome, as the mark for it and the colour that mark carries.
    status: Option<(IconName, Hsla)>,
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
            preview: None,
            status: None,
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

    pub(super) fn preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }

    pub(super) fn status(mut self, icon: IconName, color: Hsla) -> Self {
        self.status = Some((icon, color));
        self
    }

    pub(super) fn accessible_label(mut self, label: impl Into<String>) -> Self {
        self.accessible_label = label.into();
        self
    }

    pub(super) fn render(self, cx: &mut Context<TranscriptView>) -> Stateful<Div> {
        let colors = self.tone.colors(cx);
        let expandable = self.expanded.is_some();
        // The chevron alone: what it means is already carried by the card it
        // heads, and a word beside it repeats that in the widest slot of the
        // row. The state assistive technology reads stays in the row label.
        let chevron = self.expanded.map(|expanded| {
            Icon::new(match expanded {
                true => IconName::ChevronDown,
                false => IconName::ChevronRight,
            })
            .size(px(AGENT_CARD_HINT_SIZE))
            .text_color(cx.theme().muted_foreground.opacity(0.8))
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
        h_flex()
            .id(self.id)
            .w_full()
            .gap(px(AGENT_CARD_GAP))
            .items_center()
            .min_h(px(AGENT_CARD_HEADER_HEIGHT))
            .px(px(AGENT_CARD_PADDING_X))
            .py(px(AGENT_CARD_PADDING_Y))
            .line_height(relative(AGENT_CARD_LINE_HEIGHT))
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
            // The title takes the row now that nothing follows it, rather
            // than the half it shared with the command line: a purpose the
            // harness wrote, or a list of edited paths, is what identifies a
            // collapsed step.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(AGENT_CARD_TITLE_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(label_color)
                    .child(self.label),
            )
            .children(self.preview.map(|preview| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(AGENT_CARD_DETAIL_SIZE))
                    .text_color(cx.theme().muted_foreground.opacity(0.8))
                    .child(preview)
            }))
            .children(self.status.map(|(icon, color)| {
                Icon::new(icon)
                    .size(px(AGENT_CARD_DETAIL_SIZE))
                    .text_color(color)
            }))
            .children(chevron)
    }
}
