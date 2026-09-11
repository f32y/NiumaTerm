use nmt_agent::input_history::AgentInputHistory as InputHistoryService;

#[cfg(test)]
mod tests;

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::{env, io, process};

use gpui::{App, Context, Entity, Global, Window};
use gpui_component::input::TextareaState;
pub(crate) use nmt_agent::input_history::InputHistoryScope;

use crate::AgentPane;

pub(crate) struct AgentInputHistory(InputHistoryService);

impl Global for AgentInputHistory {}

impl AgentInputHistory {
    fn entries(&self, scope: &InputHistoryScope) -> Arc<[String]> {
        self.0.entries(scope)
    }

    fn record(&mut self, scope: &InputHistoryScope, text: String) -> bool {
        self.0.record(scope, text)
    }
}

pub fn initialize(testing: bool, cx: &mut App) {
    cx.set_global(AgentInputHistory(InputHistoryService::open(
        history_file_path(testing),
    )));
}

pub fn flush(cx: &App) -> io::Result<()> {
    cx.global::<AgentInputHistory>().0.flush()
}

fn history_file_path(testing: bool) -> PathBuf {
    if testing {
        env::temp_dir()
            .join("NiumaTerm")
            .join(format!("input-history-testing-{}", process::id()))
            .join("agent-input-history.json")
    } else {
        nmt_config::config_dir_path().join("agent-input-history.json")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputHistoryDirection {
    Older,
    Newer,
}

#[derive(Debug, PartialEq, Eq)]
enum InputHistoryAction {
    Declined,
    Keep,
    Replace(String),
    Clear,
}

#[derive(Default)]
pub(crate) struct InputHistoryNavigation {
    entries: Arc<[String]>,
    index: Option<usize>,
}

impl InputHistoryNavigation {
    pub(crate) fn reset(&mut self) {
        self.entries = [].into();
        self.index = None;
    }

    fn navigate(
        &mut self,
        direction: InputHistoryDirection,
        text: &str,
        selection: Range<usize>,
        cursor: usize,
        available: Arc<[String]>,
    ) -> InputHistoryAction {
        if let Some(index) = self.index {
            let Some(recalled) = self.entries.get(index) else {
                self.reset();

                return InputHistoryAction::Declined;
            };

            if text != recalled {
                self.reset();
            } else {
                if !selection.is_empty() || (cursor != 0 && cursor != text.len()) {
                    return InputHistoryAction::Declined;
                }

                return match direction {
                    InputHistoryDirection::Older if index > 0 => {
                        let index = index - 1;

                        self.index = Some(index);

                        InputHistoryAction::Replace(self.entries[index].clone())
                    }

                    InputHistoryDirection::Older => InputHistoryAction::Keep,

                    InputHistoryDirection::Newer if index + 1 < self.entries.len() => {
                        let index = index + 1;

                        self.index = Some(index);

                        InputHistoryAction::Replace(self.entries[index].clone())
                    }

                    InputHistoryDirection::Newer => {
                        self.reset();

                        InputHistoryAction::Clear
                    }
                };
            }
        }

        if direction == InputHistoryDirection::Newer || !text.is_empty() || !selection.is_empty() {
            return InputHistoryAction::Declined;
        }

        let Some(index) = available.len().checked_sub(1) else {
            return InputHistoryAction::Declined;
        };

        let text = available[index].clone();

        self.entries = available;
        self.index = Some(index);

        InputHistoryAction::Replace(text)
    }
}

impl AgentPane {
    pub(crate) fn handle_input_history_control(
        &mut self,
        direction: InputHistoryDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (text, selection, cursor) = {
            let input = self.input.read(cx);

            (
                input.text().to_string(),
                input.selected_range(),
                input.cursor(),
            )
        };

        let available = cx
            .global::<AgentInputHistory>()
            .entries(&self.input_history_scope);

        let action = self
            .input_history_navigation
            .navigate(direction, &text, selection, cursor, available);

        match action {
            InputHistoryAction::Declined => false,

            InputHistoryAction::Keep => {
                cx.stop_propagation();

                true
            }

            InputHistoryAction::Replace(text) => {
                self.palette.reset_for_recall();
                replace_input_with_history(&self.input, text, window, cx);
                cx.stop_propagation();

                cx.notify();

                true
            }

            InputHistoryAction::Clear => {
                self.input
                    .update(cx, |input, cx| input.set_value("", window, cx));

                cx.stop_propagation();

                cx.notify();

                true
            }
        }
    }

    pub(super) fn record_input_history(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.trim();

        if text.is_empty() {
            return;
        }

        cx.global_mut::<AgentInputHistory>()
            .record(&self.input_history_scope, text.to_string());

        self.input_history_navigation.reset();
    }
}

fn replace_input_with_history<T: 'static>(
    input: &Entity<TextareaState>,
    text: String,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let end = text.len();

    input.update(cx, |input, cx| {
        input.set_value(text, window, cx);
        input.set_selected_range(end..end, cx);
    });
}
