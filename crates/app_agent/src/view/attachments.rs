use gpui::{ObjectFit, img};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tooltip::Tooltip;

use crate::composer::attachments::Attachment;
use crate::*;

/// Edge of a thumbnail. Large enough to recognize a screenshot by, small
/// enough that a full message's worth of them does not push the composer off
/// the pane.
const THUMBNAIL: f32 = 56.0;
const ANNOTATION_WIDTH: f32 = 240.0;
const ANNOTATION_PREVIEW_CHARS: usize = 160;

impl AgentPane {
    /// The images the pending message carries, above the composer text they
    /// are anchored in. Absent while nothing is attached, so an ordinary
    /// message keeps the composer where it has always been.
    pub(in crate::view) fn render_attachments(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.attachments.is_empty() && self.response_annotations.is_empty() {
            return None;
        }

        Some(
            h_flex()
                .id("agent-attachments")
                .aria_label(i18n("agent-composer-context-label"))
                .w_full()
                .px_3()
                .pt_3()
                .gap_2()
                .flex_wrap()
                .children(
                    self.attachments
                        .iter()
                        .enumerate()
                        .map(|(index, attachment)| self.render_attachment(index, attachment, cx)),
                )
                .children(
                    self.response_annotations
                        .iter()
                        .enumerate()
                        .map(|(index, text)| self.render_response_annotation(index, text, cx)),
                )
                .into_any_element(),
        )
    }

    /// One thumbnail with the control that takes it back off. The image
    /// renders from the bytes the paste produced, so no file is written for
    /// something the user may still remove.
    fn render_attachment(
        &self,
        index: usize,
        attachment: &Attachment,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(("agent-attachment", index))
            .group("agent-attachment")
            .relative()
            .size(px(THUMBNAIL))
            .flex_none()
            .rounded(UI_RADIUS)
            .overflow_hidden()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .aria_label(attachment.placeholder().to_string())
            .child(
                img(attachment.image())
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .invisible()
                    .group_hover("agent-attachment", |this| this.visible())
                    .child(
                        Button::new(("agent-attachment-remove", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .aria_label(i18n("agent-composer-image-remove"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_attachment(index, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// One annotation, as its own chip. They stay separate rather than folding
    /// into a single count because each one is a different quotation the user
    /// chose, and one of them being wrong is a reason to drop that one.
    ///
    /// The chip shows as much as fits and carries the whole selection in its
    /// tooltip: a quotation is often several lines, and a strip that grew to
    /// hold them would take the composer's room.
    fn render_response_annotation(
        &self,
        index: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut chars = text.chars();
        let mut preview: String = chars.by_ref().take(ANNOTATION_PREVIEW_CHARS).collect();
        if chars.next().is_some() {
            preview.push('…');
        }
        let label =
            i18n("agent-composer-annotation-item").replace("{index}", &(index + 1).to_string());
        let group = SharedString::from(format!("agent-response-annotation-{index}"));
        let full = SharedString::from(text.to_string());

        div()
            .id(("agent-response-annotation", index))
            .group(group.clone())
            .relative()
            .w(px(ANNOTATION_WIDTH))
            .max_w_full()
            .h(px(THUMBNAIL))
            .flex_none()
            .rounded(UI_RADIUS)
            .overflow_hidden()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .aria_label(format!("{label}: {text}"))
            .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
            .child(
                v_flex()
                    .size_full()
                    .px_2()
                    .py_1p5()
                    .pr_7()
                    .overflow_hidden()
                    .gap_0p5()
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(label))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(preview),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .invisible()
                    .group_hover(group, |this| this.visible())
                    .child(
                        Button::new(("agent-response-annotation-remove", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .aria_label(i18n("agent-composer-annotations-remove"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_response_annotation(index, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}
