use std::time::Instant;

use gpui::prelude::*;
use gpui::{Context, Window};
use gpui_component::input::{InputEvent, InputState, TextareaState};
use nmt_agent::AgentEventKind;
use nmt_agent::chat::{Question, QuestionInput, QuestionMode};
use nmt_agent::session::controller::QuestionSubmission;
use nmt_agent::session::input::{QuestionAction, QuestionCompletion, QuestionKey};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::composer::PaletteControl;
use crate::agent_tab::questions::{
    QuestionEditor, QuestionEditorState, QuestionPresentation, QuestionStatus,
};

impl AgentPane {
    pub(crate) fn present_questions(&mut self, index: usize, cx: &mut Context<Self>) {
        self.prompts.reveal(&self.session.borrow().input, index);

        let shared = self.session.clone();
        let state = shared.borrow();
        let prompt = &state.input.batches()[index];
        let waiting = prompt.mode() != QuestionMode::Async;

        let description = prompt
            .questions()
            .first()
            .map(|question| question.question.clone())
            .unwrap_or_default();

        if waiting {
            self.emit_lifecycle(
                AgentEventKind::PermissionRequested,
                &t!("agent-session-needs-input", name = self.kind.display()),
                &description,
                cx,
            );
        }

        cx.notify();
    }

    pub(crate) fn present_question_completion(
        &mut self,
        completion: QuestionCompletion,
        cx: &mut Context<Self>,
    ) {
        if completion.started_turn {
            self.start_working(cx);
            self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
        }

        self.prompts.hide_settled(&self.session.borrow().input);

        if completion.waiting_finished {
            self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
        }

        self.transcript.update(cx, |_, cx| cx.notify());

        cx.notify();
    }

    pub(crate) fn open_message_questions(
        &mut self,
        item_id: &str,
        questions: Vec<Question>,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        self.prompts
            .open_history(&mut self.session.borrow_mut().input, item_id, questions);

        cx.notify();
    }

    pub(crate) fn toggle_question_option(
        &mut self,
        question: usize,
        option: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        if let Some(prompt) = self
            .prompts
            .questions_mut(&mut self.session.borrow_mut().input)
        {
            prompt.toggle(question, option);

            if let Some(active) = self.prompts.active
                && let Some(presentation) = self.prompts.presentations.get_mut(&active)
            {
                presentation.focus = (question, option);
            }

            cx.notify();
        }
    }

    pub(crate) fn handle_question_control(
        &mut self,
        control: PaletteControl,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        if self.prompts.collapsed {
            return false;
        }

        let Some(key) = self.prompts.active else {
            return false;
        };

        let mut state = self.session.borrow_mut();

        let Some(prompt) = state.input.draft_mut(key) else {
            return false;
        };

        if prompt.mode() == QuestionMode::Async || prompt.status() != QuestionStatus::Pending {
            return false;
        }

        let Some(presentation) = self.prompts.presentations.get_mut(&key) else {
            return false;
        };

        let handled = match control {
            PaletteControl::Previous => presentation.move_focus(prompt, false),
            PaletteControl::Next => presentation.move_focus(prompt, true),

            PaletteControl::Activate => {
                let (question, option) = presentation.focus;

                if prompt
                    .questions()
                    .get(question)
                    .and_then(|question| question.options.get(option))
                    .is_none()
                {
                    return false;
                }

                prompt.toggle(question, option);

                true
            }

            PaletteControl::Complete | PaletteControl::Dismiss => false,
        };

        if handled {
            cx.stop_propagation();

            cx.notify();
        }

        handled
    }

    pub(crate) fn submit_current_questions(&mut self, cx: &mut Context<Self>) {
        let key = self
            .prompts
            .questions(&self.session.borrow().input)
            .map(|prompt| prompt.key());

        if let Some(key) = key {
            self.submit_question(key, QuestionAction::Answer, cx);
        }
    }

    pub(crate) fn skip_current_questions(&mut self, cx: &mut Context<Self>) {
        let key = self
            .prompts
            .questions(&self.session.borrow().input)
            .map(|prompt| prompt.key());

        if let Some(key) = key {
            self.submit_question(key, QuestionAction::Skip, cx);
        }
    }

    fn submit_question(
        &mut self,
        key: QuestionKey,
        action: QuestionAction,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        let outcome = self
            .session
            .borrow_mut()
            .submit_question(key, action, Instant::now());

        match outcome {
            QuestionSubmission::Ignored => return,

            QuestionSubmission::Settled { waiting_finished } => {
                if waiting_finished {
                    self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                }
            }

            QuestionSubmission::Waiting | QuestionSubmission::Failed => {}
        }

        self.prompts.hide_settled(&self.session.borrow().input);

        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn restore_question_drafts(&mut self) {
        self.session.borrow_mut().restore_questions();
        self.prompts.reset_editors();
    }

    pub(crate) fn prepare_question_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompts.collapsed {
            return;
        }

        let Some(batch) = self.prompts.active else {
            return;
        };

        let shared = self.session.clone();
        let state = shared.borrow();

        let Some(prompt) = state.input.draft(batch) else {
            return;
        };

        let count = prompt.questions().len();

        self.prompts
            .presentations
            .entry(batch)
            .or_insert_with(|| QuestionPresentation::new(prompt));

        drop(state);

        for index in 0..count {
            let shared = self.session.clone();
            let state = shared.borrow();

            let Some(prompt) = state.input.draft(batch) else {
                return;
            };

            let input = prompt.questions()[index].input;

            if input == QuestionInput::SelectionOnly
                || self.prompts.presentations[&batch].editors[index].is_some()
                || !prompt.pending()
            {
                continue;
            }

            let text = prompt.text(index).to_string();
            let key = prompt.key();
            let epoch = self.session.borrow().runtime.epoch();

            let on_change = move |this: &mut Self, value: String, cx: &mut Context<Self>| {
                if !this.binding.is_current() || !this.session.borrow().runtime.is_current(epoch) {
                    return;
                }

                let mut state = this.session.borrow_mut();

                let Some(prompt) = state.input.draft_mut(key) else {
                    return;
                };

                if !prompt.set_text(index, value) {
                    return;
                }

                cx.notify();
            };

            let (state, subscription) = if input == QuestionInput::Secret {
                let state = cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(true)
                        .placeholder(t!("agent-question-free-text"))
                        .default_value(text)
                });

                let subscription = cx.subscribe(&state, move |this, input, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        on_change(this, input.read(cx).value().to_string(), cx);
                    }
                });

                (QuestionEditorState::Secret(state), subscription)
            } else {
                let state = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(1, 4)
                        .placeholder(t!("agent-question-free-text"))
                        .default_value(text)
                });

                let subscription = cx.subscribe(&state, move |this, input, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        on_change(this, input.read(cx).value().to_string(), cx);
                    }
                });

                (QuestionEditorState::Text(state), subscription)
            };

            if let Some(presentation) = self.prompts.presentations.get_mut(&batch) {
                presentation.editors[index] = Some(QuestionEditor {
                    state,
                    _subscription: subscription,
                });
            }
        }
    }
}
