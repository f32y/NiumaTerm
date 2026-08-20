use gpui::{ObjectFit, img};
use gpui_component::button::{Button, ButtonVariants};

use crate::agent::composer::attachments::Attachment;
use crate::agent::*;

/// Edge of a thumbnail. Large enough to recognize a screenshot by, small
/// enough that a full message's worth of them does not push the composer off
/// the pane.
const THUMBNAIL: f32 = 56.0;

impl AgentPane {
    /// The images the pending message carries, above the composer text they
    /// are anchored in. Absent while nothing is attached, so an ordinary
    /// message keeps the composer where it has always been.
    pub(in crate::agent::view) fn render_attachments(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.attachments.is_empty() {
            return None;
        }

        Some(
            h_flex()
                .id("agent-attachments")
                .aria_label(i18n("agent-composer-images-label"))
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
}
