pub(super) use nmt_agent::input_history::InputHistoryScope;

#[cfg(test)]
#[path = "input_history_tests.rs"]
mod input_history_tests;

use crate::agent_tab::AgentPane;
use gpui::{App, Context, Entity, Global};
use gpui_component::input::TextareaState;
use nmt_agent::input_history::AgentInputHistory as InputHistoryService;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::{env, io, process};

pub(super) struct AgentInputHistory(pub(super) InputHistoryService);

impl Global for AgentInputHistory {}

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
pub(super) enum InputHistoryDirection {
    Older,
    Newer,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum InputHistoryAction {
    Declined,
    Keep,
    Replace(String),
    Clear,
}

#[derive(Default)]
pub(super) struct InputHistoryNavigation {
    entries: Arc<[String]>,
    index: Option<usize>,
}

impl InputHistoryNavigation {
    pub(super) fn reset(&mut self) {
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

    pub(super) fn record_input_history(
        &mut self,
        scope: &InputHistoryScope,
        text: &str,
        cx: &mut Context<AgentPane>,
    ) {
        let text = text.trim();

        if text.is_empty() {
            return;
        }

        cx.global_mut::<AgentInputHistory>()
            .0
            .record(scope, text.to_string());

        self.reset();
    }

    pub(super) fn navigate_input(
        &mut self,
        direction: InputHistoryDirection,
        input: &Entity<TextareaState>,
        scope: &InputHistoryScope,
        cx: &App,
    ) -> InputHistoryAction {
        let (text, selection, cursor) = {
            let input = input.read(cx);

            (
                input.text().to_string(),
                input.selected_range(),
                input.cursor(),
            )
        };

        let available = cx.global::<AgentInputHistory>().0.entries(scope);

        self.navigate(direction, &text, selection, cursor, available)
    }
}
