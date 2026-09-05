use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, MouseButton, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::Input;
use gpui_component::radio::Radio;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use nmt_agent_utils::chat::{QuestionInput, QuestionMode};
use nmt_i18n::i18n;

use crate::AgentPane;
use crate::questions::QuestionStatus;
use crate::session::Status;
use crate::settings::UI_RADIUS;

impl AgentPane {
    pub(crate) fn render_question_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let count = self.prompts.pending_count();
        if self.prompts.collapsed && count == 0 {
            return None;
        }
        self.prepare_question_editors(window, cx);
        let active = self.prompts.active?;
        let prompt = self.prompts.questions()?;
        let collapsed = self.prompts.collapsed;
        let pending = prompt.pending();
        let enabled = prompt.status == QuestionStatus::Pending
            && matches!(self.runtime.status, Status::Idle | Status::Running)
            && self.runtime.update_suspension.is_none()
            && !self.branch_flow_holds_composer()
            && !self.palette.awaiting_command_turn
            && !self
                .prompts
                .batches
                .iter()
                .any(|prompt| prompt.status == QuestionStatus::Submitting);
        let status = match prompt.status {
            QuestionStatus::Pending => {
                if prompt.mode == QuestionMode::Async {
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
        let count_label = i18n("agent-question-count").replace("{count}", &count.to_string());
        let mut heading = h_flex().w_full().items_center().gap_2().child(
            div()
                .flex_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(if count > 0 {
                    count_label
                } else {
                    i18n(status).to_string()
                }),
        );
        let candidates: Vec<usize> = self
            .prompts
            .batches
            .iter()
            .enumerate()
            .filter_map(|(index, prompt)| (prompt.pending() || index == active).then_some(index))
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
                .label(i18n(if collapsed {
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
                    if let Some(prompt) = this.prompts.questions_mut() {
                        prompt.touch();
                    }
                    cx.notify();
                }),
            )
            .capture_key_down(cx.listener(|this, _, _, cx| {
                if let Some(prompt) = this.prompts.questions_mut() {
                    prompt.touch();
                }
                cx.notify();
            }));
        if collapsed {
            return Some(panel.into_any_element());
        }

        let mut rows = Vec::new();
        for (index, question) in prompt.questions.iter().enumerate() {
            let group: SharedString = format!("question-{active}-{index}").into();
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
                            prompt.is_focused(index, option_index)
                                && prompt.mode != QuestionMode::Async
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
                            .label(i18n("agent-question-custom"))
                            .checked(prompt.custom[index])
                            .disabled(!enabled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(prompt) = this.prompts.questions_mut() {
                                    prompt.custom[index] = true;
                                    prompt.touch();
                                    if let Some(editor) = &prompt.editors[index] {
                                        editor
                                            .state
                                            .update(cx, |input, cx| input.focus(window, cx));
                                    }
                                    cx.notify();
                                }
                            })),
                    );
                }
                if pending {
                    if let Some(editor) = &prompt.editors[index] {
                        row = row.child(Input::new(&editor.state).disabled(!enabled));
                    }
                } else if prompt.status == QuestionStatus::Submitted && prompt.custom[index] {
                    row = row.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if question.input == QuestionInput::Secret {
                                i18n("agent-question-secret-submitted").to_string()
                            } else {
                                prompt.text[index].clone()
                            }),
                    );
                }
            }
            rows.push(row.into_any_element());
        }
        panel = panel.child(
            v_flex()
                .id(("question-scroll", active))
                .w_full()
                .max_h((window.viewport_size().height * 0.4).min(px(280.)))
                .overflow_y_scroll()
                .gap_3()
                .children(rows),
        );
        if let Some(error) = &prompt.error {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if let Some(remaining) = prompt
            .auto_resolve_remaining()
            .filter(|remaining| remaining.as_secs() <= 60)
        {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        i18n("agent-question-timeout")
                            .replace("{seconds}", &remaining.as_secs().to_string()),
                    ),
            );
        }
        let mut footer = h_flex().w_full().items_center().gap_2().child(
            div()
                .flex_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(i18n(status)),
        );
        if pending {
            footer = footer
                .child(
                    Button::new("question-skip")
                        .ghost()
                        .disabled(!enabled)
                        .label(i18n(if prompt.mode == QuestionMode::Async {
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
                        .label(i18n("agent-question-submit"))
                        .on_click(cx.listener(|this, _, _, cx| this.submit_current_questions(cx))),
                );
        }
        Some(panel.child(footer).into_any_element())
    }
}
