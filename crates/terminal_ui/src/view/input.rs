use std::ops::Range;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, EntityInputHandler, ExternalPaths, KeyDownEvent, Keystroke, Modifiers,
    Pixels, Point, UTF16Selection, Window, point, px, size,
};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use nmt_i18n::i18n;
use nmt_terminal::session::interaction::PendingCopy;
use tracing::warn;

use crate::pane_model::key_action::{KeyOutcome, TextInput};
use crate::view::key::terminal_key;
use crate::view::{AgentInterrupted, SendShiftTab, SendTab, TerminalPane};

struct TextCopiedNotification;

fn show_text_copied(window: &mut Window, cx: &mut App) {
    window.push_notification(
        Notification::new()
            .message(i18n("terminal-text-copied"))
            .id::<TextCopiedNotification>()
            .autohide_after(Duration::from_millis(1500))
            .show_close(false)
            .w_auto()
            .px_3()
            .py_2(),
        cx,
    );
}

impl TerminalPane {
    pub(super) fn begin_copy(
        &mut self,
        copy: PendingCopy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| match copy.request.await {
            Ok(Ok(text)) => {
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.model.finish_copy(text, copy.completion) {
                        show_text_copied(window, cx);
                        this.invalidate(cx);
                        cx.notify();
                    }
                });
            }
            result => warn!("terminal copy did not complete: {result:?}"),
        })
        .detach();
    }

    /// UI reaction to input reaching the PTY: optionally snap the view back
    /// to the latest output.
    fn react_to_pty_input(&mut self, cx: &mut Context<Self>) {
        if self.model.settings.scroll_to_bottom_when_typing {
            self.scroll_to_latest(cx);
        }
    }

    pub(super) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let interrupts_agent = matches!(event.keystroke.key.as_str(), "escape" | "esc")
            && !event.keystroke.modifiers.modified();

        match self.model.key_down(&terminal_key(&event.keystroke)) {
            KeyOutcome::Ignored => return,
            KeyOutcome::Scrolled(outcome) => {
                self.apply_scroll_outcome(outcome, cx);
                return;
            }
            KeyOutcome::Written => self.react_to_pty_input(cx),
            KeyOutcome::CopyPending(copy) => {
                self.begin_copy(copy, window, cx);
                return;
            }
        }

        if interrupts_agent {
            cx.emit(AgentInterrupted);
        }

        self.invalidate(cx);
    }

    /// Route a keystroke straight to the terminal PTY.
    pub(crate) fn feed_terminal_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        match self.model.send_key(&terminal_key(keystroke)) {
            KeyOutcome::Ignored => return,
            KeyOutcome::Scrolled(outcome) => {
                self.apply_scroll_outcome(outcome, cx);
                return;
            }
            KeyOutcome::Written => self.react_to_pty_input(cx),
            KeyOutcome::CopyPending(copy) => {
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(text)) = copy.request.await {
                        let _ = this.update(cx, |this, cx| {
                            if this.model.finish_copy(text, copy.completion) {
                                this.invalidate(cx);
                                cx.notify();
                            }
                        });
                    }
                })
                .detach();
            }
        }

        self.invalidate(cx);
    }

    /// Tab/Shift-Tab belong to the shell (completion) while the terminal is
    /// focused, but `Root` binds them to focus traversal and key bindings
    /// dispatch before the pane's `on_key_down` listener. These actions are
    /// bound in the deeper `Terminal` context, which wins over `Root`.
    pub(super) fn on_send_tab(&mut self, _: &SendTab, _: &mut Window, cx: &mut Context<Self>) {
        self.feed_terminal_key(
            &Keystroke {
                modifiers: Modifiers::none(),
                key: "tab".into(),
                key_char: None,
            },
            cx,
        );
    }

    pub(super) fn on_send_shift_tab(
        &mut self,
        _: &SendShiftTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.feed_terminal_key(
            &Keystroke {
                modifiers: Modifiers::shift(),
                key: "tab".into(),
                key_char: None,
            },
            cx,
        );
    }

    pub(super) fn on_file_drop(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);

        if self
            .model
            .write_text_input(TextInput::DropPaths(paths.paths()))
        {
            self.invalidate(cx);
        }
    }
}

/// Commit-only IME: composition and candidate placement stay with the OS; the
/// pane receives only the committed string. Inline preedit stays in the IME-owned UI,
/// marked-text methods are inert. `bounds_for_range` reports the terminal cursor
/// cell so the OS positions the candidate window correctly.
impl EntityInputHandler for TerminalPane {
    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model.write_text_input(TextInput::Commit(text)) {
            self.react_to_pty_input(cx);
            self.invalidate(cx);
        }
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self.model.frame_cache.current()?.cursor()?;
        let cell = self.model.cell_metrics?;

        // `element_bounds` is the terminal leaf's content rect (padding already
        // excluded), so the cursor cell offsets from its origin directly — plus
        // the inter-block gap offset for the cursor's row.
        let cursor_y = self.model.viewport.cursor_y(cursor.row, cell.height_px);

        Some(Bounds::new(
            point(
                element_bounds.left() + px(cursor.col as f32 * cell.width_px),
                element_bounds.top() + px(cursor_y),
            ),
            size(px(cell.width_px), px(cell.height_px)),
        ))
    }

    // No editable document and no preedit: text and marked-text methods are inert.
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // GPUI's Windows IME path queries bounds only after obtaining a
        // selection; an empty virtual caret keeps commit-only input eligible.
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        _new_text: &str,
        _new_selected: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
