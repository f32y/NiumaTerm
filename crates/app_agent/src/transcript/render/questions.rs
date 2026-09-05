use gpui::prelude::*;
use gpui::{AnyElement, Context, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, v_flex};
use nmt_agent_utils::chat::Question;
use nmt_i18n::i18n;

use crate::settings::UI_RADIUS;
use crate::transcript::TranscriptView;

impl TranscriptView {
    pub(super) fn render_question_message(
        &self,
        index: usize,
        id: String,
        text: String,
        questions: Vec<Question>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let owner = self.owner().cloned();
        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded(UI_RADIUS)
            .border_1()
            .border_color(cx.theme().border)
            .child(self.render_agent_row(index, text, cx))
            .child(div().children(owner.map(|owner| {
                Button::new(("message-questions", index))
                    .ghost()
                    .small()
                    .label(i18n("agent-question-open"))
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |pane, cx| {
                            pane.open_message_questions(&id, questions.clone(), cx)
                        });
                    })
            })))
            .into_any_element()
    }
}
