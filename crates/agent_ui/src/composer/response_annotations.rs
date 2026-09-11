use gpui::{Context, Window};
use gpui_base::TextSelection;
pub(crate) use nmt_agent::annotations::{
    parse_annotated_prompt, prompt_with_response_annotations, visible_prompt,
};

use crate::AgentPane;

pub(crate) fn annotation_count_label(count: usize) -> String {
    let key = if count == 1 {
        "agent-composer-annotation-count-one"
    } else {
        "agent-composer-annotation-count-other"
    };

    nmt_i18n::i18n(key).replace("{count}", &count.to_string())
}

impl AgentPane {
    pub(crate) fn add_response_annotation(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.attachments.add_annotation(text) {
            return;
        }

        TextSelection::clear(window, cx);
        self.focus(window, cx);

        cx.notify();
    }

    pub(crate) fn remove_response_annotation(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.attachments.remove_annotation(index) {
            cx.notify();
        }
    }
}
