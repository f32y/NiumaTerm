use gpui::{ObjectFit, img};
use gpui_component::button::{Button, ButtonVariants};

use crate::agent::composer::annotation_count_label;
use crate::agent::composer::attachments::Attachment;
use crate::agent::*;

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
    pub(in crate::agent::view) fn render_attachments(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
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
                    (!self.response_annotations.is_empty())
                        .then(|| self.render_response_annotations(cx)),
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

    fn render_response_annotations(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.response_annotations.join(" · ");
        let mut chars = text.chars();
        let mut preview: String = chars.by_ref().take(ANNOTATION_PREVIEW_CHARS).collect();
        if chars.next().is_some() {
            preview.push('…');
        }
        let label = annotation_count_label(self.response_annotations.len());

        div()
            .id("agent-response-annotations")
            .group("agent-response-annotations")
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
                    .group_hover("agent-response-annotations", |this| this.visible())
                    .child(
                        Button::new("agent-response-annotations-remove")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .aria_label(i18n("agent-composer-annotations-remove"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.clear_response_annotations(cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}
