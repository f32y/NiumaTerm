use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, MouseButton, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{InputEvent, InputState, TextareaState};
use gpui_component::radio::Radio;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use nmt_agent::chat::{Question, QuestionInput, QuestionMode};
use nmt_agent::session::controller::SessionController;
use nmt_agent::session::input::{
    QuestionDraft, QuestionError, QuestionKey, QuestionStatus, SessionInput,
};
use rust_i18n::t;

use crate::agent_tab::questions::{QuestionEditor, QuestionEditorState, QuestionPresentation};
use crate::agent_tab::settings::UI_RADIUS;
use crate::agent_tab::{AgentPane, PaletteControl};

/// The card that asks the user an agent's questions: which batch of questions
/// it shows, whether it is folded away, and the editors and keyboard focus
/// each batch keeps while it is on screen.
#[derive(Default)]
pub(crate) struct QuestionPanel {
    pub(crate) presentations: HashMap<QuestionKey, QuestionPresentation>,
    pub(crate) active: Option<QuestionKey>,
    pub(crate) collapsed: bool,
}

impl QuestionPanel {
    pub(crate) fn questions<'a>(&self, input: &'a SessionInput) -> Option<&'a QuestionDraft> {
        self.active.and_then(|key| input.draft(key))
    }

    pub(crate) fn questions_mut<'a>(
        &mut self,
        input: &'a mut SessionInput,
    ) -> Option<&'a mut QuestionDraft> {
        input.draft_mut(self.active?)
    }

    pub(crate) fn questions_open(&self, input: &SessionInput) -> bool {
        !self.collapsed && self.questions(input).is_some()
    }

    pub(crate) fn reveal(&mut self, input: &SessionInput, index: usize) {
        let Some(prompt) = input.batches().get(index) else {
            return;
        };

        let key = prompt.key();

        let reveal = self.presentations.contains_key(&key)
            || self.questions(input).is_none_or(|question| {
                !question.pending()
                    || (prompt.mode() != QuestionMode::Async
                        && question.mode() == QuestionMode::Async)
            });

        self.presentations
            .retain(|key, _| input.draft(*key).is_some());

        self.presentations
            .entry(key)
            .or_insert_with(|| QuestionPresentation::new(prompt));

        if reveal {
            self.active = Some(key);
            self.collapsed = false;
        }
    }

    pub(crate) fn open_history(
        &mut self,
        input: &mut SessionInput,
        item_id: &str,
        questions: Vec<Question>,
    ) {
        let index = input.history(item_id, questions);

        let prompt = &input.batches()[index];
        let key = prompt.key();

        self.presentations
            .entry(key)
            .or_insert_with(|| QuestionPresentation::new(prompt));

        self.active = Some(key);
        self.collapsed = false;
    }

    /// Completed batches release the panel; explicitly opened history stays visible.
    pub(crate) fn hide_settled(&mut self, input: &SessionInput) {
        let settled = self
            .questions(input)
            .is_none_or(|prompt| !prompt.pending() && prompt.status() != QuestionStatus::History);

        if settled {
            self.active = input
                .batches()
                .iter()
                .find(|draft| draft.pending())
                .map(|draft| draft.key());
        }

        self.release_secret_editors(input);
    }

    pub(crate) fn release_secret_editors(&mut self, input: &SessionInput) {
        self.presentations.retain(|key, presentation| {
            let Some(draft) = input.draft(*key) else {
                return false;
            };

            if !draft.pending() {
                for (question, editor) in draft.questions().iter().zip(&mut presentation.editors) {
                    if question.input == QuestionInput::Secret {
                        *editor = None;
                    }
                }
            }

            true
        });
    }

    pub(crate) fn reset_editors(&mut self) {
        for presentation in self.presentations.values_mut() {
            for editor in &mut presentation.editors {
                *editor = None;
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.presentations.clear();

        self.active = None;
        self.collapsed = false;
    }

    /// Flip option `option` of question `question` in the shown batch and
    /// move the keyboard focus there. Returns whether a batch took it.
    pub(crate) fn toggle_option(
        &mut self,
        input: &mut SessionInput,
        question: usize,
        option: usize,
    ) -> bool {
        let Some(prompt) = self.questions_mut(input) else {
            return false;
        };

        prompt.toggle(question, option);

        if let Some(active) = self.active
            && let Some(presentation) = self.presentations.get_mut(&active)
        {
            presentation.focus = (question, option);
        }

        true
    }

    /// Answer a navigation key for a batch the turn is waiting on. Returns
    /// whether the panel took the key.
    pub(crate) fn handle_control(
        &mut self,
        control: PaletteControl,
        input: &mut SessionInput,
    ) -> bool {
        if self.collapsed {
            return false;
        }

        let Some(key) = self.active else {
            return false;
        };

        let Some(prompt) = input.draft_mut(key) else {
            return false;
        };

        if prompt.mode() == QuestionMode::Async || prompt.status() != QuestionStatus::Pending {
            return false;
        }

        let Some(presentation) = self.presentations.get_mut(&key) else {
            return false;
        };

        match control {
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
        }
    }

    /// Build the text and secret editors the shown batch still lacks. They are
    /// made ahead of rendering because creating one needs the pane's context
    /// while the render borrows the session.
    pub(crate) fn prepare_editors(
        &mut self,
        session: &Rc<RefCell<SessionController>>,
        window: &mut Window,
        cx: &mut Context<AgentPane>,
    ) {
        if self.collapsed {
            return;
        }

        let Some(batch) = self.active else {
            return;
        };

        let shared = session.clone();
        let state = shared.borrow();

        let Some(prompt) = state.input().draft(batch) else {
            return;
        };

        let count = prompt.questions().len();

        self.presentations
            .entry(batch)
            .or_insert_with(|| QuestionPresentation::new(prompt));

        drop(state);

        for index in 0..count {
            let shared = session.clone();
            let state = shared.borrow();

            let Some(prompt) = state.input().draft(batch) else {
                return;
            };

            let input = prompt.questions()[index].input;

            if input == QuestionInput::SelectionOnly
                || self.presentations[&batch].editors[index].is_some()
                || !prompt.pending()
            {
                continue;
            }

            let text = prompt.text(index).to_string();
            let key = prompt.key();
            let epoch = session.borrow().runtime().epoch();

            let on_change = move |this: &mut AgentPane,
                                  value: String,
                                  cx: &mut Context<AgentPane>| {
                if !this.binding.is_current() || !this.session.borrow().runtime().is_current(epoch)
                {
                    return;
                }

                let mut state = this.session.borrow_mut();

                let Some(prompt) = state.input_mut().draft_mut(key) else {
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

            if let Some(presentation) = self.presentations.get_mut(&batch) {
                presentation.editors[index] = Some(QuestionEditor::new(state, subscription));
            }
        }
    }

    /// The panel for the shown batch. `composer_free` is whether no branch
    /// or rewind flow holds the composer, which answering also needs.
    pub(crate) fn render(
        &mut self,
        session: &Rc<RefCell<SessionController>>,
        composer_free: bool,
        window: &mut Window,
        cx: &mut Context<AgentPane>,
    ) -> Option<AnyElement> {
        let count = session.borrow().input().pending_count();

        if self.collapsed && count == 0 {
            return None;
        }

        self.prepare_editors(session, window, cx);

        let active = self.active?;
        let shared = session.clone();
        let state = shared.borrow();
        let prompt = self.questions(state.input())?;
        let collapsed = self.collapsed;
        let pending = prompt.pending();

        let enabled = session.borrow().can_submit_question(prompt.key())
            && composer_free
            && !session.borrow().commands().awaiting_turn;

        let presentation = self.presentations.get(&active)?;

        let status = match prompt.status() {
            QuestionStatus::Pending => {
                if prompt.mode() == QuestionMode::Async {
                    "agent-question-async"
                } else {
                    "agent-question-pending"
                }
            }
            QuestionStatus::Submitting => "agent-question-submitting",
            QuestionStatus::Submitted => "agent-question-submitted",
            QuestionStatus::Skipped => "agent-question-skipped",
            QuestionStatus::Expired => "agent-question-expired",
            QuestionStatus::History => "agent-question-history",
        };

        let count_label = t!("agent-question-count", count = count).into_owned();

        let mut heading = h_flex().w_full().items_center().gap_2().child(
            div()
                .flex_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(if count > 0 {
                    count_label
                } else {
                    t!(status).to_string()
                }),
        );

        let candidates: Vec<QuestionKey> = session
            .borrow()
            .input()
            .batches()
            .iter()
            .filter_map(|prompt| {
                (prompt.pending() || prompt.key() == active).then_some(prompt.key())
            })
            .collect();

        if candidates.len() > 1 {
            let position = candidates
                .iter()
                .position(|index| *index == active)
                .unwrap_or(0);

            let previous = candidates[(position + candidates.len() - 1) % candidates.len()];
            let next = candidates[(position + 1) % candidates.len()];

            heading = heading
                .child(
                    Button::new("question-previous-batch")
                        .ghost()
                        .small()
                        .label("<")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.prompts.active = Some(previous);
                            this.prompts.collapsed = false;

                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .child(format!("{} / {}", position + 1, candidates.len())),
                )
                .child(
                    Button::new("question-next-batch")
                        .ghost()
                        .small()
                        .label(">")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.prompts.active = Some(next);
                            this.prompts.collapsed = false;

                            cx.notify();
                        })),
                );
        }

        heading = heading.child(
            Button::new("question-collapse")
                .ghost()
                .small()
                .label(t!(if collapsed {
                    "agent-question-open"
                } else {
                    "agent-question-collapse"
                }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.prompts.collapsed = !this.prompts.collapsed;

                    cx.notify();
                })),
        );

        let mut panel = v_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.65))
            .bg(cx.theme().muted.opacity(0.2))
            .child(heading)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if let Some(prompt) = this
                        .prompts
                        .questions_mut(this.session.borrow_mut().input_mut())
                    {
                        prompt.touch();
                    }

                    cx.notify();
                }),
            )
            .capture_key_down(cx.listener(|this, _, _, cx| {
                if let Some(prompt) = this
                    .prompts
                    .questions_mut(this.session.borrow_mut().input_mut())
                {
                    prompt.touch();
                }

                cx.notify();
            }));

        if collapsed {
            return Some(panel.into_any_element());
        }

        let mut rows = Vec::new();

        for (index, question) in prompt.questions().iter().enumerate() {
            let group: SharedString = format!("question-{active:?}-{index}").into();

            let mut row = v_flex()
                .w_full()
                .gap_1p5()
                .children(
                    question
                        .header
                        .as_ref()
                        .filter(|header| !header.is_empty())
                        .map(|header| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(header.clone())
                        }),
                )
                .child(div().text_sm().child(question.question.clone()));

            for (option_index, option) in question.options.iter().enumerate() {
                let label = option
                    .description
                    .as_ref()
                    .filter(|description| !description.is_empty())
                    .map_or_else(
                        || option.label.clone(),
                        |description| format!("{} — {description}", option.label),
                    );

                let control = if question.multi_select {
                    Checkbox::new((group.clone(), option_index))
                        .label(label)
                        .checked(prompt.is_selected(index, option_index))
                        .disabled(!enabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_question_option(index, option_index, cx)
                        }))
                        .into_any_element()
                } else {
                    Radio::new((group.clone(), option_index))
                        .label(label)
                        .checked(prompt.is_selected(index, option_index))
                        .disabled(!enabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_question_option(index, option_index, cx)
                        }))
                        .into_any_element()
                };

                row = row.child(
                    div()
                        .w_full()
                        .px_1p5()
                        .py_0p5()
                        .rounded(UI_RADIUS)
                        .when(
                            presentation.is_focused(index, option_index)
                                && prompt.mode() != QuestionMode::Async
                                && enabled,
                            |this| this.bg(cx.theme().list_active),
                        )
                        .child(control),
                );
            }

            if question.input != QuestionInput::SelectionOnly {
                if !question.options.is_empty() {
                    row = row.child(
                        Radio::new((group.clone(), question.options.len()))
                            .label(t!("agent-question-custom").into_owned())
                            .checked(prompt.is_custom(index))
                            .disabled(!enabled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(prompt) = this
                                    .prompts
                                    .questions_mut(this.session.borrow_mut().input_mut())
                                {
                                    if !prompt.choose_custom(index) {
                                        return;
                                    }

                                    if let Some(active) = this.prompts.active
                                        && let Some(presentation) =
                                            this.prompts.presentations.get(&active)
                                        && let Some(editor) = &presentation.editors[index]
                                    {
                                        editor.focus(window, cx);
                                    }

                                    cx.notify();
                                }
                            })),
                    );
                }

                if pending {
                    if let Some(editor) = &presentation.editors[index] {
                        row = row.child(editor.render(!enabled));
                    }
                } else if prompt.status() == QuestionStatus::Submitted && prompt.is_custom(index) {
                    row = row.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if question.input == QuestionInput::Secret {
                                t!("agent-question-secret-submitted").to_string()
                            } else {
                                prompt.text(index).to_string()
                            }),
                    );
                }
            }

            rows.push(row.into_any_element());
        }

        panel = panel.child(
            v_flex()
                .id(SharedString::from(format!("question-scroll-{active:?}")))
                .w_full()
                .max_h((window.viewport_size().height * 0.4).min(px(280.)))
                .overflow_y_scroll()
                .gap_3()
                .children(rows),
        );

        if let Some(error) = prompt.error() {
            let error = match error {
                QuestionError::Disconnected => t!("agent-question-disconnected").to_string(),
                QuestionError::Rejected(message) => message.clone(),
            };

            panel = panel.child(div().text_sm().text_color(cx.theme().danger).child(error));
        }

        if let Some(remaining) = prompt
            .auto_resolve_remaining(Instant::now())
            .filter(|remaining| remaining.as_secs() <= 60)
        {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        t!("agent-question-timeout", seconds = remaining.as_secs()).into_owned(),
                    ),
            );
        }

        let mut footer = h_flex().w_full().items_center().gap_2().child(
            div()
                .flex_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(t!(status)),
        );

        if pending {
            footer = footer
                .child(
                    Button::new("question-skip")
                        .ghost()
                        .disabled(!enabled)
                        .label(t!(if prompt.mode() == QuestionMode::Async {
                            "agent-question-dismiss"
                        } else {
                            "agent-question-skip"
                        }))
                        .on_click(cx.listener(|this, _, _, cx| this.skip_current_questions(cx))),
                )
                .child(
                    Button::new("question-submit")
                        .primary()
                        .disabled(!enabled || !prompt.is_complete())
                        .label(t!("agent-question-submit"))
                        .on_click(cx.listener(|this, _, _, cx| this.submit_current_questions(cx))),
                );
        }

        Some(panel.child(footer).into_any_element())
    }
}
