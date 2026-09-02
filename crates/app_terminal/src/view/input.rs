use nmt_i18n::i18n;

use crate::input as terminal_input;
use crate::view::*;

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

pub(super) fn should_scroll_to_latest(keystroke: &Keystroke, alt_screen: bool) -> bool {
    !alt_screen && !keystroke.modifiers.modified() && keystroke.key.eq_ignore_ascii_case("end")
}

pub(super) fn dropped_paths_text(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| {
            let path = path.to_string_lossy();
            if path.contains(' ') {
                format!("\"{path}\"")
            } else {
                path.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl TerminalPane {
    /// UI reaction to input reaching the PTY: optionally snap the view back
    /// to the latest output.
    fn react_to_pty_input(&mut self, cx: &mut Context<Self>) {
        if cx.global::<TerminalSettings>().scroll_to_bottom_when_typing {
            self.scroll_to_latest(cx);
        }
    }

    pub(super) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if should_scroll_to_latest(&event.keystroke, self.surface.alt_screen())
            && self.scroll_to_latest(cx)
        {
            return;
        }

        // Plain printable text arrives through the char/IME path
        // (`replace_text_in_range`); encoding it here too would double it.
        if terminal_input::should_defer_to_ime(&event.keystroke) {
            return;
        }

        let action = terminal_input::key_action(
            &event.keystroke,
            cx.global::<TerminalSettings>().newline_shortcut,
        );
        let interrupts_agent = matches!(event.keystroke.key.as_str(), "escape" | "esc")
            && !event.keystroke.modifiers.modified();

        // Block-split: copy the frozen-region selection on the copy chord.
        if let (SurfaceKeyAction::CopyOrWrite(_), Some((a, b))) =
            (&action, self.frozen_drag.current())
        {
            let text = self.frozen_selection_to_text(a, b);
            if !text.is_empty() {
                self.surface.copy_text_to_clipboard(text);
                show_text_copied(window, cx);
                self.frozen_drag.clear();
                cx.notify();
                return;
            }
        }

        match self.surface.apply_key_action(action) {
            SurfaceKeyResult::Ignored => return,
            SurfaceKeyResult::Copied => show_text_copied(window, cx),
            SurfaceKeyResult::Handled => self.react_to_pty_input(cx),
        }

        if interrupts_agent {
            cx.emit(AgentInterrupted);
        }

        self.invalidate(cx);
    }

    /// Route a keystroke straight to the terminal PTY.
    pub(crate) fn feed_terminal_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        match self.surface.apply_key_action(terminal_input::key_action(
            keystroke,
            cx.global::<TerminalSettings>().newline_shortcut,
        )) {
            SurfaceKeyResult::Ignored => return,
            SurfaceKeyResult::Handled => self.react_to_pty_input(cx),
            SurfaceKeyResult::Copied => {}
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

        if self.surface.paste_text(&dropped_paths_text(paths.paths())) {
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
        if text.is_empty() {
            return;
        }

        if self.surface.write_text(text) {
            self.react_to_pty_input(cx);
            self.invalidate(cx);
        }
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self.frame_cache.current()?.cursor()?;
        let cell = self.cell_metrics?;

        // `element_bounds` is the terminal leaf's content rect (padding already
        // excluded), so the cursor cell offsets from its origin directly — plus
        // the inter-block gap offset for the cursor's row.
        let offsets = self.current_row_offsets(cx);

        let mut y_offset = row_y_offset(&offsets, cursor.row as usize);

        // Block list: the live grid starts at `active_top` in the list.
        if self.block_list_mode(cx) {
            y_offset += self.frozen.active_top();
        }

        Some(Bounds::new(
            point(
                element_bounds.left() + px(cursor.col as f32 * cell.width_px),
                element_bounds.top() + px(cursor.row as f32 * cell.height_px + y_offset),
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
