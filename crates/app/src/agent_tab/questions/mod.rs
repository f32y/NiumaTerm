//! Editors and keyboard focus for core-owned question drafts.

pub(super) use nmt_agent::session::input::QuestionStatus;

#[cfg(test)]
mod tests;

use gpui::{AnyElement, App, Entity, IntoElement as _, Subscription, Window};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use nmt_agent::session::input::QuestionDraft;

pub(super) struct QuestionEditor {
    state: QuestionEditorState,
    _subscription: Subscription,
}

pub(super) enum QuestionEditorState {
    Text(Entity<TextareaState>),
    Secret(Entity<InputState>),
}

impl QuestionEditorState {
    fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Text(state) => state.update(cx, |state, cx| state.focus(window, cx)),
            Self::Secret(state) => state.update(cx, |state, cx| state.focus(window, cx)),
        }
    }

    fn render(&self, disabled: bool) -> AnyElement {
        match self {
            Self::Text(state) => Textarea::new(state).disabled(disabled).into_any_element(),
            Self::Secret(state) => Input::new(state).disabled(disabled).into_any_element(),
        }
    }
}

pub(super) struct QuestionPresentation {
    pub(super) editors: Vec<Option<QuestionEditor>>,
    pub(super) focus: (usize, usize),
}

impl QuestionPresentation {
    pub(super) fn new(draft: &QuestionDraft) -> Self {
        Self {
            editors: (0..draft.questions().len()).map(|_| None).collect(),
            focus: (0, 0),
        }
    }

    pub(super) fn is_focused(&self, question: usize, option: usize) -> bool {
        self.focus == (question, option)
    }

    pub(super) fn move_focus(&mut self, draft: &mut QuestionDraft, forward: bool) -> bool {
        if draft.status() != QuestionStatus::Pending {
            return false;
        }

        let order: Vec<(usize, usize)> = draft
            .questions()
            .iter()
            .enumerate()
            .flat_map(|(question, entry)| {
                (0..entry.options.len()).map(move |option| (question, option))
            })
            .collect();

        if order.is_empty() {
            return false;
        }

        draft.touch();

        let Some(current) = order.iter().position(|entry| *entry == self.focus) else {
            self.focus = order[0];

            return true;
        };

        let next = if forward {
            (current + 1) % order.len()
        } else {
            (current + order.len() - 1) % order.len()
        };

        self.focus = order[next];

        true
    }
}

impl QuestionEditor {
    pub(super) fn new(state: QuestionEditorState, subscription: Subscription) -> Self {
        Self {
            state,
            _subscription: subscription,
        }
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.focus(window, cx);
    }

    pub(super) fn render(&self, disabled: bool) -> AnyElement {
        self.state.render(disabled)
    }
}
