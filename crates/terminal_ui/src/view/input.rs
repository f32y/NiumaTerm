use std::time::Duration;

use gpui::prelude::*;
use gpui::{App, Window};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use nmt_i18n::i18n;

struct TextCopiedNotification;

pub(super) fn show_text_copied(window: &mut Window, cx: &mut App) {
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
