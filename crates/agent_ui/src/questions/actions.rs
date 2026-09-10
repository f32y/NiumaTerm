use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{Context, Window};
use gpui_component::input::{InputEvent, InputState, TextareaState};
use nmt_agent::AgentEventKind;
use nmt_agent::chat::{
    Item, Question, QuestionInput, QuestionMode, QuestionRequest, QuestionResolution,
};
use nmt_agent::session::input::{QuestionAction, QuestionKey, Submission};
use nmt_i18n::i18n;

use crate::AgentPane;
use crate::composer::PaletteControl;
use crate::questions::{QuestionEditor, QuestionEditorState, QuestionStatus};
use crate::session::Status;

impl AgentPane {
    pub(crate) fn receive_questions(&mut self, request: QuestionRequest, cx: &mut Context<Self>) {
        let optional = request.mode == QuestionMode::Optional;
        let waiting = request.mode != QuestionMode::Async;
        let Some(index) = self.prompts.receive(&self.runtime, request) else {
            return;
        };
        let prompt = &self.prompts.core.batches()[index];
        let key = prompt.key();
        let description = prompt
            .questions()
            .first()
            .map(|question| question.question.clone())
            .unwrap_or_default();

        if waiting {
            self.emit_lifecycle(
                AgentEventKind::PermissionRequested,
                &i18n("agent-session-needs-input").replace("{name}", self.kind.display()),
                &description,
                cx,
            );
        }

        cx.notify();

        if !optional {
            return;
        }

        let epoch = self.runtime.epoch();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let keep_running = this.update(cx, |this, cx| {
                    if !this.runtime.is_current(epoch) || this.runtime.update_suspension().is_some()
                    {
                        return false;
                    }

                    let Some(prompt) = this
                        .prompts
                        .core
                        .batches()
                        .iter()
                        .find(|prompt| prompt.key() == key)
                    else {
                        return false;
                    };
                    let Some(remaining) = prompt.auto_resolve_remaining(Instant::now()) else {
                        return false;
                    };
                    if remaining.is_zero() {
                        this.submit_question(key, QuestionAction::Timeout, cx);
                        return false;
                    }

                    cx.notify();

                    true
                });

                if !keep_running.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn resolve_questions(
        &mut self,
        id: &str,
        resolution: QuestionResolution,
        cx: &mut Context<Self>,
    ) {
        let Some(completion) = self
            .prompts
            .core
            .resolve(self.runtime.epoch(), id, resolution)
        else {
            return;
        };
        if let Some(text) = completion.message {
            if completion.started_turn && self.runtime.status() == Status::Idle {
                self.delivery.begin_turn();
                self.start_working(cx);
                self.runtime.turn_started();
                self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
            }
            self.push_item(Item::UserMessage { text: Some(text) }, cx);
        }
        self.prompts.hide_settled();
        if completion.waiting_finished {
            self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
        }

        self.transcript.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub(crate) fn question_submission_failed(
        &mut self,
        id: &str,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if self
            .prompts
            .core
            .submission_failed(self.runtime.epoch(), id, message)
        {
            cx.notify();
        }
    }

    pub(crate) fn open_message_questions(
        &mut self,
        item_id: &str,
        questions: Vec<Question>,
        cx: &mut Context<Self>,
    ) {
        self.prompts.open_history(item_id, questions);
        cx.notify();
    }

    pub(crate) fn toggle_question_option(
        &mut self,
        question: usize,
        option: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(prompt) = self.prompts.questions_mut() {
            prompt.toggle(question, option);
            if let Some(active) = self.prompts.active {
                self.prompts.presentations[active].focus = (question, option);
            }
            cx.notify();
        }
    }

    pub(crate) fn handle_question_control(
        &mut self,
        control: PaletteControl,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.prompts.collapsed {
            return false;
        }

        let Some(index) = self.prompts.active else {
            return false;
        };
        let key = self.prompts.core.batches()[index].key();
        let Some(prompt) = self.prompts.core.draft_mut(key) else {
            return false;
        };
        if prompt.mode() == QuestionMode::Async || prompt.status() != QuestionStatus::Pending {
            return false;
        }
        let presentation = &mut self.prompts.presentations[index];
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
        if let Some(prompt) = self.prompts.questions() {
            self.submit_question(prompt.key(), QuestionAction::Answer, cx);
        }
    }

    pub(crate) fn skip_current_questions(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.prompts.questions() {
            self.submit_question(prompt.key(), QuestionAction::Skip, cx);
        }
    }

    fn submit_question(
        &mut self,
        key: QuestionKey,
        action: QuestionAction,
        cx: &mut Context<Self>,
    ) {
        if self.branch_flow_holds_composer() || self.palette.commands.awaiting_turn {
            return;
        }
        let waiting = self.prompts.core.waiting();
        match self.prompts.core.submit(
            &mut self.runtime,
            key,
            action,
            &self.controls.state.settings,
            Instant::now(),
        ) {
            Submission::Ignored => return,
            Submission::Settled if waiting && !self.prompts.core.waiting() => {
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
            }
            Submission::Settled | Submission::Waiting | Submission::Failed => {}
        }
        self.prompts.hide_settled();
        cx.notify();
    }

    pub(crate) fn restore_question_drafts(&mut self) {
        self.prompts.core.restore(&mut self.runtime);
        self.prompts.reset_editors();
    }

    pub(crate) fn prepare_question_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompts.collapsed {
            return;
        }

        let Some(batch) = self.prompts.active else {
            return;
        };
        let count = self.prompts.core.batches()[batch].questions().len();

        for index in 0..count {
            let prompt = &self.prompts.core.batches()[batch];
            let input = prompt.questions()[index].input;

            if input == QuestionInput::SelectionOnly
                || self.prompts.presentations[batch].editors[index].is_some()
                || !prompt.pending()
            {
                continue;
            }

            let text = prompt.text(index).to_string();
            let key = prompt.key();
            let epoch = self.runtime.epoch();

            let on_change = move |this: &mut Self, value: String, cx: &mut Context<Self>| {
                if !this.runtime.is_current(epoch) {
                    return;
                }

                let Some(prompt) = this.prompts.core.draft_mut(key) else {
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
                        .placeholder(i18n("agent-question-free-text"))
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
                        .placeholder(i18n("agent-question-free-text"))
                        .default_value(text)
                });

                let subscription = cx.subscribe(&state, move |this, input, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        on_change(this, input.read(cx).value().to_string(), cx);
                    }
                });

                (QuestionEditorState::Text(state), subscription)
            };

            self.prompts.presentations[batch].editors[index] = Some(QuestionEditor {
                state,
                _subscription: subscription,
            });
        }
    }
}
