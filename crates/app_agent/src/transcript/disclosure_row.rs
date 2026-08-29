use gpui::transparent_black;

use crate::transcript::view::TranscriptView;
use crate::*;

/// Card geometry for the transcript's expandable rows. Every tool call, work
/// run and structural break in the conversation is drawn as one card, so these
/// are the numbers that keep their headers on a single baseline grid.
pub(super) const AGENT_CARD_RADIUS: f32 = 6.0;
pub(super) const AGENT_CARD_PADDING_X: f32 = 8.0;
pub(super) const AGENT_CARD_PADDING_Y: f32 = 4.0;
/// Resting height of a card header. A floor rather than a fixed height: the
/// row keeps one rhythm down a run of steps, and still grows for a label that
/// wraps at a narrow pane width.
pub(super) const AGENT_CARD_HEADER_HEIGHT: f32 = 24.0;
/// Vertical inset of a card's body. Looser than the header's, because the
/// header is one line of labels while the body is a block of output that
/// needs air between it and the rule above it.
pub(super) const AGENT_CARD_BODY_PADDING_Y: f32 = 8.0;
pub(super) const AGENT_CARD_GAP: f32 = 9.0;
/// The square that holds a row's type icon, and the icon drawn inside it. A
/// work-log row draws the icon bare, so the block is only a fixed slot that
/// keeps every title in a run starting on the same column; a failed step
/// still fills it, and its radius applies there.
pub(super) const AGENT_CARD_ICON_BLOCK: f32 = 14.0;
pub(super) const AGENT_CARD_ICON_BLOCK_RADIUS: f32 = 5.0;
pub(super) const AGENT_CARD_ICON: f32 = 13.0;
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
/// How far a work-log title is let down from the conversation's own text. The
/// steps are what the reply was assembled from rather than the reply itself,
/// so they read one shade quieter than the prose around them.
const AGENT_CARD_TITLE_FADE: f32 = 0.65;
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
            // Drawn with no border, fill or icon plate at rest. A run of
            // steps is process metadata around the conversation, and a stack
            // of outlined boxes competes with the prose it documents for the
            // reader's attention; the run is grouped by the rule down its
            // left edge instead, and only the pointed-at row takes a fill.
            Self::Neutral => AgentCardColors {
                border: transparent_black(),
                background: transparent_black(),
                icon_block: transparent_black(),
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
        // Only a failed step is drawn as a card. Giving the neutral tone a
        // transparent border instead would still inset its contents by a
        // pixel on every side, which shows up as a break in the rule that
        // groups a run.
        .when(tone == AgentCardTone::Failed, |this| {
            this.border_1()
                .border_color(colors.border)
                .bg(colors.background)
        })
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
            .unwrap_or_else(|| cx.theme().foreground.opacity(AGENT_CARD_TITLE_FADE));
        let type_icon = self.type_icon.map(|icon| {
            Icon::new(icon)
                .size(px(AGENT_CARD_ICON))
                .text_color(icon_color)
        });
        h_flex()
            .id(self.id)
            // A work-log row is only as wide as what it says, so the hover
            // fill wraps the words instead of running the width of the
            // column. A failed step keeps the full width its card needs.
            .map(|this| match self.tone {
                AgentCardTone::Failed => this.w_full(),
                AgentCardTone::Neutral => this.self_start().max_w_full(),
            })
            .gap(px(AGENT_CARD_GAP))
            .items_center()
            // The hover fill is the row's only resting edge, so it carries
            // the card radius here rather than inheriting it from a border
            // the neutral tone no longer draws.
            .rounded(px(AGENT_CARD_RADIUS))
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
            // The slot exists only for a row that has an icon to put in it.
            // Reserving it for one that does not leaves the label hanging off
            // an empty indent with nothing above or below to line up with.
            .children(type_icon.map(|icon| {
                div()
                    .size(px(AGENT_CARD_ICON_BLOCK))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(AGENT_CARD_ICON_BLOCK_RADIUS))
                    .bg(colors.icon_block)
                    .child(icon)
            }))
            // The title sizes to its own text and the outcome mark and
            // chevron follow it directly, so the eye reads label and state
            // together instead of tracking across the column to a right
            // edge. It still shrinks and truncates in a narrow pane.
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(AGENT_CARD_TITLE_SIZE))
                    .text_color(label_color)
                    .child(self.label),
            )
            .children(self.status.map(|(icon, color)| {
                Icon::new(icon)
                    .flex_none()
                    .size(px(AGENT_CARD_DETAIL_SIZE))
                    .text_color(color)
            }))
            .children(chevron.map(|chevron| chevron.flex_none()))
            .children(self.preview.map(|preview| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(AGENT_CARD_DETAIL_SIZE))
                    .text_color(cx.theme().muted_foreground.opacity(0.8))
                    .child(preview)
            }))
    }
}
