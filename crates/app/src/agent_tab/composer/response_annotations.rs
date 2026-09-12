use gpui::{Context, Window};
use gpui_base::TextSelection;
pub(in crate::agent_tab) use nmt_agent::annotations::{
    parse_annotated_prompt, prompt_with_response_annotations, visible_prompt,
};
use rust_i18n::t;

use crate::agent_tab::AgentPane;

pub(in crate::agent_tab) fn annotation_count_label(count: usize) -> String {
    let key = if count == 1 {
        "agent-composer-annotation-count-one"
    } else {
        "agent-composer-annotation-count-other"
    };

    t!(key, count = count).into_owned()
}

impl AgentPane {
    pub(in crate::agent_tab) fn add_response_annotation(
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

    pub(in crate::agent_tab) fn remove_response_annotation(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if self.attachments.remove_annotation(index) {
            cx.notify();
        }
    }
}
