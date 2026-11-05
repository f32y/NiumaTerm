mod view;

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, Context, EventEmitter, FocusHandle, IntoElement, ListState, Pixels, Point, Render,
    Window,
};
use nmt_agent::chat::Item;
use nmt_agent::transcript::conversation::ConversationState;
use rust_i18n::t;

use crate::agent_tab::composer::parse_annotated_prompt;
use crate::agent_tab::transcript::RowSpec;
use crate::agent_tab::transcript::navigation::view::render;
use crate::agent_tab::transcript::rows::TranscriptRow;

pub(super) struct Navigate {
    pub(super) epoch: u64,
    pub(super) entry: usize,
    pub(super) version: (u64, u64),
}

struct Message {
    entry: usize,
    row: Option<usize>,
    preview: String,
}

pub(super) struct Navigation {
    messages: Vec<Message>,
    version: Option<(u64, u64)>,
    epoch: u64,
    current: Option<usize>,
    hovered: Option<usize>,
    keyboard_selection: Option<usize>,
    pointer: Option<Point<Pixels>>,
    focus: FocusHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    list: ListState,
}

impl EventEmitter<Navigate> for Navigation {}

impl Navigation {
    pub(super) fn new(list: ListState, cx: &mut Context<Self>) -> Self {
        Self {
            messages: Vec::new(),
            version: None,
            epoch: 0,
            current: None,
            hovered: None,
            keyboard_selection: None,
            pointer: None,
            focus: cx.focus_handle(),
            bounds: Rc::new(Cell::new(None)),
            list,
        }
    }

    pub(super) fn sync(
        &mut self,
        conversation: &ConversationState,
        rows: &[TranscriptRow],
        row_start: usize,
        epoch: u64,
        cx: &mut Context<Self>,
    ) {
        let change = self
            .version
            .and_then(|version| conversation.changes_since(version));

        let reset = self.epoch != epoch
            || change
                .as_ref()
                .and_then(|change| change.reset_from)
                .is_some_and(|first| {
                    self.messages
                        .last()
                        .is_some_and(|message| first <= message.entry)
                });

        let version = conversation.version();
        let previous_current = self.current;

        let mut changed = reset;

        let first = if reset {
            0
        } else if self.version.is_some() {
            change.map_or(conversation.content.entries().len(), |change| change.first)
        } else {
            0
        };

        if reset || self.version != Some(version) {
            let keep = self
                .messages
                .partition_point(|message| message.entry < first);

            changed |= keep != self.messages.len();

            self.messages.truncate(keep);

            for (entry, content) in conversation
                .content
                .entries()
                .iter()
                .enumerate()
                .skip(first)
            {
                if let Item::UserMessage { text } = &content.item {
                    changed = true;

                    let parsed = text.as_deref().and_then(parse_annotated_prompt);

                    let text = parsed
                        .as_ref()
                        .map_or(text.as_deref().unwrap_or_default(), |p| p.prompt);

                    let preview = text
                        .split_whitespace()
                        .flat_map(|word| word.chars().chain([' ']))
                        .take(240)
                        .collect::<String>();

                    self.messages.push(Message {
                        entry,
                        row: None,
                        preview: if preview.trim().is_empty() {
                            t!("agent-navigation-attachment").into_owned()
                        } else {
                            preview.trim_end().to_owned()
                        },
                    });
                }
            }

            if reset {
                self.hovered = None;
                self.keyboard_selection = None;
                self.pointer = None;
            }

            self.version = Some(version);
            self.epoch = epoch;
        }

        // Physical row numbers change when replies split into Markdown blocks.
        // Resolve them from entry positions after the transcript updates its rows.
        let row_start = if reset { 0 } else { row_start };

        let mut message = self
            .messages
            .partition_point(|message| message.row.is_some_and(|row| row < row_start));

        for (row, content) in rows.iter().enumerate().skip(row_start) {
            if let RowSpec::Entry { index, .. } = content.spec {
                while message < self.messages.len() && self.messages[message].entry < index {
                    changed |= self.messages[message].row.is_some();
                    self.messages[message].row = None;
                    message += 1;
                }

                if let Some(target) = self.messages.get_mut(message)
                    && target.entry == index
                {
                    changed |= target.row != Some(row);
                    target.row = Some(row);
                    message += 1;
                }
            }
        }

        for target in &mut self.messages[message..] {
            changed |= target.row.is_some();
            target.row = None;
        }

        self.messages.retain(|message| message.row.is_some());

        self.update_current();

        if changed || previous_current != self.current {
            cx.notify();
        }
    }

    pub(super) fn version(&self) -> Option<(u64, u64)> {
        self.version
    }

    fn observe_scroll(&mut self, cx: &mut Context<Self>) {
        let old = self.current;

        self.update_current();

        if old != self.current {
            cx.notify();
        }
    }

    fn update_current(&mut self) {
        let top = self.list.logical_scroll_top().item_ix;

        let next = self
            .messages
            .partition_point(|message| message.row.is_none_or(|row| row < top));

        let viewport = self.list.viewport_bounds();

        self.current = if self
            .messages
            .get(next)
            .and_then(|m| m.row)
            .and_then(|row| self.list.bounds_for_item(row))
            .is_some_and(|bounds| {
                bounds.top() < viewport.bottom() && bounds.bottom() > viewport.top()
            }) {
            Some(next)
        } else {
            next.checked_sub(1)
        };
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(message) = self.messages.get(index)
            && message.row.is_some()
        {
            cx.emit(Navigate {
                epoch: self.epoch,
                entry: message.entry,
                version: self.version.expect("indexed conversation version"),
            });
        }
    }

    fn index_at(&self, y: Pixels) -> Option<usize> {
        let bounds = self.bounds.get()?;

        if self.messages.is_empty() || bounds.size.height <= Pixels::ZERO {
            return None;
        }

        let fraction = ((y - bounds.top()) / bounds.size.height).clamp(0., 1.);

        Some((fraction * self.messages.len().saturating_sub(1) as f32).round() as usize)
    }
}

impl Render for Navigation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render(self, window, cx)
    }
}
