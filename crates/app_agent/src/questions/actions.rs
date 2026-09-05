use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Window};
use gpui_component::input::{InputEvent, InputState};
use nmt_agent_utils::AgentEventKind;
use nmt_agent_utils::chat::{
    Item, Question, QuestionInput, QuestionMode, QuestionRequest, QuestionResolution,
};
use nmt_i18n::i18n;

use crate::AgentPane;
use crate::composer::PaletteControl;
use crate::questions::{QuestionEditor, QuestionPrompt, QuestionStatus};
use crate::session::Status;

impl AgentPane {
    pub(crate) fn receive_questions(&mut self, request: QuestionRequest, cx: &mut Context<Self>) {
        let id = request.id.clone();
        if self.prompts.batches.iter().any(|prompt| {
            prompt.id.as_deref() == Some(id.as_str()) && prompt.status != QuestionStatus::History
        }) {
            return;
        }
        let optional = request.mode == QuestionMode::Optional;
        let waiting = request.mode != QuestionMode::Async;
        let description = request
            .questions
            .first()
            .map(|question| question.question.clone())
            .unwrap_or_default();
        let mut prompt = QuestionPrompt::from_request(request);
        prompt.thread_id = self
            .runtime
            .backend
            .as_ref()
            .and_then(|backend| backend.recovery_identity())
            .map(|identity| identity.id);
        self.prompts.ask_questions(prompt);
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
        let epoch = self.runtime.epoch;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep_running = this.update(cx, |this, cx| {
                    if this.runtime.epoch != epoch || this.runtime.update_suspension.is_some() {
                        return false;
                    }
                    let Some(index) = this
                        .prompts
                        .batches
                        .iter()
                        .position(|prompt| prompt.id.as_deref() == Some(id.as_str()))
                    else {
                        return false;
                    };
                    let Some(remaining) = this.prompts.batches[index].auto_resolve_remaining()
                    else {
                        return false;
                    };
                    if remaining.is_zero() {
                        this.submit_question(index, None, cx);
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
        let Some(index) = self
            .prompts
            .batches
            .iter()
            .position(|prompt| prompt.id.as_deref() == Some(id))
        else {
            return;
        };
        let waiting = self.prompts.batches[index].mode != QuestionMode::Async;
        let status = match resolution {
            QuestionResolution::Submitted {
                message,
                started_turn,
            } => {
                if let Some(text) = message {
                    if started_turn && self.runtime.status == Status::Idle {
                        self.turn.seq += 1;
                        self.start_working(cx);
                        self.runtime.status = Status::Running;
                        self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
                    }
                    self.push_item(Item::UserMessage { text: Some(text) }, cx);
                }
                QuestionStatus::Submitted
            }
            QuestionResolution::Skipped => QuestionStatus::Skipped,
            QuestionResolution::Expired => QuestionStatus::Expired,
        };
        self.prompts.batches[index].settle(status);
        if waiting
            && !self
                .prompts
                .batches
                .iter()
                .any(|prompt| prompt.pending() && prompt.mode != QuestionMode::Async)
            && !self.prompts.approval_open()
        {
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
        if let Some(prompt) = self
            .prompts
            .batches
            .iter_mut()
            .find(|prompt| prompt.id.as_deref() == Some(id))
        {
            prompt.status = QuestionStatus::Pending;
            prompt.error = Some(message);
            prompt.touch();
        }
        cx.notify();
    }

    pub(crate) fn open_message_questions(
        &mut self,
        item_id: &str,
        questions: Vec<Question>,
        cx: &mut Context<Self>,
    ) {
        let id = format!("message:{item_id}");
        let index = match self
            .prompts
            .batches
            .iter()
            .position(|prompt| prompt.id.as_deref() == Some(id.as_str()))
        {
            Some(index) => index,
            None => {
                let mut prompt = QuestionPrompt::from_request(QuestionRequest {
                    id,
                    questions,
                    mode: QuestionMode::Async,
                });
                prompt.status = QuestionStatus::History;
                prompt.selected.iter_mut().for_each(Vec::clear);
                self.prompts.batches.push(prompt);
                self.prompts.batches.len() - 1
            }
        };
        self.prompts.active = Some(index);
        self.prompts.collapsed = false;
        self.prompts.batches[index].touch();
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
        let Some(prompt) = self.prompts.questions_mut() else {
            return false;
        };
        // Async drafts leave the composer's normal keyboard shortcuts available.
        if prompt.mode == QuestionMode::Async || prompt.status != QuestionStatus::Pending {
            return false;
        }
        let handled = match control {
            PaletteControl::Previous => prompt.move_focus(false),
            PaletteControl::Next => prompt.move_focus(true),
            PaletteControl::Activate => {
                let (question, option) = prompt.focus;
                if prompt
                    .questions
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
        let Some(index) = self.prompts.active else {
            return;
        };
        let prompt = &self.prompts.batches[index];
        if prompt.status != QuestionStatus::Pending || !prompt.is_complete() {
            return;
        }
        let answers = prompt.answers();
        self.submit_question(index, Some(answers), cx);
    }

    pub(crate) fn skip_current_questions(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.prompts.active {
            self.submit_question(index, None, cx);
        }
    }

    fn submit_question(
        &mut self,
        index: usize,
        answers: Option<Vec<Vec<String>>>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.runtime.status, Status::Idle | Status::Running)
            || self.runtime.update_suspension.is_some()
            || self.branch_flow_holds_composer()
            || self.palette.awaiting_command_turn
        {
            return;
        }
        let prompt = &self.prompts.batches[index];
        if prompt.status != QuestionStatus::Pending
            || self
                .prompts
                .batches
                .iter()
                .any(|prompt| prompt.status == QuestionStatus::Submitting)
        {
            return;
        }
        let id = prompt.id.clone();
        let mode = prompt.mode;
        let skipped = answers.is_none();
        let Some(backend) = self.runtime.backend.as_mut() else {
            return;
        };
        let result = match id.as_deref() {
            Some(id) => backend.respond_input(id, answers, &self.controls.settings),
            None => {
                backend.respond_questions(answers);
                Ok(())
            }
        };
        let prompt = &mut self.prompts.batches[index];
        prompt.touch();
        match result {
            Ok(()) => {
                prompt.error = None;
                if id.is_none() || (mode == QuestionMode::Async && skipped) {
                    prompt.settle(if skipped {
                        QuestionStatus::Skipped
                    } else {
                        QuestionStatus::Submitted
                    });
                    if mode != QuestionMode::Async {
                        self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                    }
                } else {
                    prompt.status = QuestionStatus::Submitting;
                }
            }
            Err(message) => {
                prompt.error = Some(message);
            }
        }
        cx.notify();
    }

    pub(crate) fn restore_question_drafts(&mut self) {
        let Some(backend) = self.runtime.backend.as_mut() else {
            return;
        };
        let thread_id = backend.recovery_identity().map(|identity| identity.id);
        let mut requests = Vec::new();
        for prompt in &mut self.prompts.batches {
            prompt.reset_editors();
            if !prompt.pending() || prompt.id.is_none() {
                continue;
            }
            if prompt.thread_id != thread_id || prompt.mode != QuestionMode::Async {
                prompt.settle(QuestionStatus::Expired);
                continue;
            }
            if prompt.status == QuestionStatus::Submitting {
                prompt.status = QuestionStatus::Pending;
                prompt.error = Some(i18n("agent-question-disconnected").to_string());
            }
            requests.push(QuestionRequest {
                id: prompt.id.clone().unwrap_or_default(),
                mode: prompt.mode,
                questions: prompt.questions.clone(),
            });
        }
        backend.restore_question_requests(requests);
    }

    pub(crate) fn prepare_question_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompts.collapsed {
            return;
        }
        let Some(batch) = self.prompts.active else {
            return;
        };
        let count = self.prompts.batches[batch].questions.len();
        for index in 0..count {
            let prompt = &self.prompts.batches[batch];
            let input = prompt.questions[index].input;
            if input == QuestionInput::SelectionOnly
                || prompt.editors[index].is_some()
                || !prompt.pending()
            {
                continue;
            }
            let text = prompt.text[index].clone();
            let state = cx.new(|cx| {
                let mut state = InputState::new(window, cx)
                    .auto_grow(1, 4)
                    .placeholder(i18n("agent-question-free-text"));
                if input == QuestionInput::Secret {
                    state = state.masked(true);
                }
                state.set_value(text, window, cx);
                state
            });
            let epoch = self.runtime.epoch;
            let subscription = cx.subscribe(&state, move |this, input, event: &InputEvent, cx| {
                if this.runtime.epoch != epoch || !matches!(event, InputEvent::Change) {
                    return;
                }
                let Some(prompt) = this.prompts.batches.get_mut(batch) else {
                    return;
                };
                if prompt.status != QuestionStatus::Pending {
                    return;
                }
                let value = input.read(cx).value().to_string();
                if prompt.text[index] == value {
                    return;
                }
                prompt.text[index] = value;
                prompt.custom[index] = true;
                prompt.touch();
                cx.notify();
            });
            self.prompts.batches[batch].editors[index] = Some(QuestionEditor {
                state,
                _subscription: subscription,
            });
        }
    }
}
