#[cfg(test)]
#[path = "find_tests.rs"]
mod tests;

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, KeyBinding, MouseButton, Render, Task, Window,
    actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Enter, Escape, Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use rust_i18n::t;

actions!(find, [OpenFind]);

pub fn bind_keys(cx: &mut App) {
    #[cfg(windows)]
    cx.bind_keys([
        KeyBinding::new(
            "ctrl-shift-f",
            OpenFind,
            Some("Terminal || (Terminal > Input)"),
        ),
        KeyBinding::new(
            "ctrl-shift-f",
            OpenFind,
            Some("AgentPane || (AgentPane > Input)"),
        ),
        KeyBinding::new("ctrl-f", OpenFind, Some("AgentPane || (AgentPane > Input)")),
    ]);

    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-f", OpenFind, Some("Terminal || (Terminal > Input)")),
        KeyBinding::new("cmd-f", OpenFind, Some("AgentPane || (AgentPane > Input)")),
    ]);
}

pub(crate) enum FindEvent {
    Editing,
    Query(String),
    Navigate { backwards: bool },
    Close,
}

pub(crate) struct FindBar {
    input: Entity<InputState>,
    pub(crate) visible: bool,
    previous_focus: Option<FocusHandle>,
    current: Option<usize>,
    count: usize,
    busy: bool,
    debounce: Option<Task<()>>,
}

impl EventEmitter<FindEvent> for FindBar {}

impl FindBar {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("find-placeholder")));

        cx.subscribe(&input, |this, input, event, cx| match event {
            InputEvent::Change => {
                this.busy = true;

                cx.emit(FindEvent::Editing);

                if input.read(cx).is_composing() {
                    this.debounce = None;

                    return;
                }

                let query = input.read(cx).value().to_string();

                this.busy = !query.is_empty();

                this.debounce = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;

                    let _ = this.update(cx, |this, cx| {
                        if this.visible {
                            cx.emit(FindEvent::Query(query));
                        }
                    });
                }));

                cx.notify();
            }
            InputEvent::PressEnter { shift, .. }
                if !this.busy && this.count > 0 && !input.read(cx).is_composing() =>
            {
                cx.emit(FindEvent::Navigate { backwards: *shift });
            }
            _ => {}
        })
        .detach();

        Self {
            input,
            visible: false,
            previous_focus: None,
            current: None,
            count: 0,
            busy: false,
            debounce: None,
        }
    }

    pub(crate) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            self.previous_focus = window.focused(cx);
            self.visible = true;

            cx.emit(FindEvent::Query(self.input.read(cx).value().to_string()));
        }

        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });

        cx.notify();
    }

    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = false;
        self.debounce = None;

        if let Some(focus) = self.previous_focus.take() {
            window.focus(&focus, cx);
        }

        cx.emit(FindEvent::Close);
        cx.notify();
    }

    pub(crate) fn results(
        &mut self,
        current: Option<usize>,
        count: usize,
        busy: bool,
        cx: &mut Context<Self>,
    ) {
        if (self.current, self.count, self.busy) != (current, count, busy) {
            self.current = current;
            self.count = count;
            self.busy = busy;

            cx.notify();
        }
    }
}

impl Render for FindBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = if self.input.read(cx).value().is_empty() {
            String::new()
        } else if self.busy {
            "...".to_owned()
        } else {
            format!(
                "{}/{}",
                self.current.map_or(0, |index| index + 1),
                self.count
            )
        };

        div()
            .id("pane-find")
            .absolute()
            .top(px(8.))
            .right(px(8.))
            .w(px(350.))
            .max_w_full()
            .h(px(36.))
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .shadow_md()
            .occlude()
            .text_size(px(12.))
            .line_height(px(20.))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close(window, cx)))
            .on_action(|_: &Enter, _, _| {})
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.input).small()),
            )
            .child(div().flex_none().child(label))
            .child(
                Button::new("find-previous")
                    .icon(IconName::ChevronUp)
                    .ghost()
                    .small()
                    .tooltip(t!("find-previous"))
                    .disabled(self.busy || self.count == 0)
                    .on_click(
                        cx.listener(|_, _, _, cx| cx.emit(FindEvent::Navigate { backwards: true })),
                    ),
            )
            .child(
                Button::new("find-next")
                    .icon(IconName::ChevronDown)
                    .ghost()
                    .small()
                    .tooltip(t!("find-next"))
                    .disabled(self.busy || self.count == 0)
                    .on_click(
                        cx.listener(|_, _, _, cx| {
                            cx.emit(FindEvent::Navigate { backwards: false })
                        }),
                    ),
            )
            .child(
                Button::new("find-close")
                    .icon(IconName::Close)
                    .ghost()
                    .small()
                    .tooltip(t!("find-close"))
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            )
    }
}

pub(crate) fn adjacent(current: Option<usize>, count: usize, backwards: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }

    Some(match current {
        Some(index) if backwards => (index + count - 1) % count,
        Some(index) => (index + 1) % count,
        None if backwards => count - 1,
        None => 0,
    })
}
