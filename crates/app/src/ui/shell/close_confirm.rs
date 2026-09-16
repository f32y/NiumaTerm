//! The dialogs that ask before a pane, tab, workspace, or window closes, and
//! the wording they share.

use std::borrow::Cow;
use std::io;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Context, Entity, SharedString, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::{
    DIALOG_BUTTON_MIN_WIDTH, Dialog, DialogButtonProps, DialogClose, DialogFooter,
};
use gpui_component::{ActiveTheme, StyledExt, WindowExt, v_flex};
use nmt_config::system::WarnBeforeTerminatingShell;
use rust_i18n::t;
use tracing::warn;

use crate::ui;
use crate::ui::settings::AppSettings;
use crate::ui::shell::AppWindow;
use crate::workspace::WorkspaceId;

/// The description of a close confirmation: `with_processes` when `count`
/// found child processes still running, `plain` otherwise.
pub(super) fn close_description(
    count: io::Result<usize>,
    plain: &str,
    with_processes: &str,
) -> String {
    match count {
        Ok(count) if count > 0 => {
            t!(with_processes, processes = &processes_running(count)).into_owned()
        }
        Ok(_) => t!(plain).into_owned(),
        Err(error) => {
            warn!("failed to count processes before closing: {error}");

            t!(plain).into_owned()
        }
    }
}

/// "1 child process is running" / "N child processes are running" — the
/// lead-in of every close-confirmation description.
fn processes_running(count: usize) -> String {
    if count == 1 {
        t!("shell-close-one-process-running").to_string()
    } else {
        t!("shell-close-many-processes-running", count = count).into_owned()
    }
}

/// Shared scaffolding of every close-confirmation alert: title +
/// description, OK runs `on_confirm` against this shell. `note` adds a
/// bold line under the description for a consequence the description
/// itself does not cover.
pub(super) fn open_close_confirm(
    window: &mut Window,
    cx: &mut Context<AppWindow>,
    // Dialog callbacks can rebuild their content, so they retain a
    // translated title that can be reused on each invocation.
    title: Cow<'static, str>,
    description: String,
    note: Option<SharedString>,
    on_confirm: impl Fn(&mut AppWindow, &mut Window, &mut Context<AppWindow>) + 'static,
) {
    let shell = cx.entity();
    let on_confirm = Rc::new(on_confirm);

    window.open_alert_dialog(cx, move |alert, _, _| {
        let shell = shell.clone();
        let on_confirm = Rc::clone(&on_confirm);

        alert
            .confirm()
            .title(title.clone())
            .description(
                v_flex()
                    .gap_1()
                    .child(description.clone())
                    .children(note.clone().map(|note| div().font_bold().child(note))),
            )
            .on_ok(move |_, window, cx| {
                let on_confirm = Rc::clone(&on_confirm);

                shell.update(cx, |this, cx| on_confirm(this, window, cx));

                true
            })
    });
}

/// The alert for a window whose settings could not be saved: closing anyway
/// discards the unsaved edits, and cancelling keeps the window open so they
/// can be retried.
pub(super) fn open_save_failed_close(
    description: String,
    note: Option<SharedString>,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) {
    // `remove_window` tears the window down directly (no WM_CLOSE
    // round-trip), so this dialog won't re-trigger.
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .title(t!("settings-save-failed-title"))
            .description(
                v_flex()
                    .gap_1()
                    .child(description.clone())
                    .children(note.clone().map(|note| div().font_bold().child(note))),
            )
            .button_props(
                DialogButtonProps::default()
                    .show_cancel(true)
                    .ok_text(t!("settings-close-without-saving"))
                    .cancel_text(t!("shell-close-cancel")),
            )
            .on_ok(|_, window, cx| {
                if cx.windows().len() == 1 {
                    cx.global_mut::<AppSettings>().discard_on_exit();
                }

                window.remove_window();

                true
            })
    });
}

pub(super) fn close_last_workspace_dialog(
    dialog: Dialog,
    shell: &Entity<AppWindow>,
    id: WorkspaceId,
    message: &str,
    note: &Option<SharedString>,
) -> Dialog {
    let quit_shell = shell.clone();
    let replace_shell = shell.clone();
    let message = message.to_string();
    let note = note.clone();

    dialog
        .title(t!("shell-close-last-workspace-title"))
        .overlay_closable(false)
        .content(move |content, _, cx| {
            content.child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.clone())
                    .children(note.clone().map(|note| div().font_bold().child(note))),
            )
        })
        .footer(
            DialogFooter::new()
                .child(
                    Button::new("replace-ws")
                        .min_w(DIALOG_BUTTON_MIN_WIDTH)
                        .label(t!("shell-close-new-default-workspace"))
                        .primary()
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);

                            replace_shell
                                .update(cx, |this, cx| this.replace_last_workspace(id, window, cx));
                        }),
                )
                .child(
                    Button::new("quit-app")
                        .min_w(DIALOG_BUTTON_MIN_WIDTH)
                        .label(t!("shell-close-quit"))
                        .danger()
                        .on_click(move |_, window, cx| {
                            if !ui::settings::save_settings(window, cx) {
                                window.close_dialog(cx);

                                return;
                            }

                            quit_shell.update(cx, |this, cx| this.doom_workspace(id, cx));

                            cx.quit();
                        }),
                )
                .child(
                    DialogClose::new().child(
                        Button::new("keep-ws")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .label(t!("shell-close-cancel")),
                    ),
                ),
        )
}

pub(super) fn should_confirm_close(
    confirm: bool,
    warn: WarnBeforeTerminatingShell,
    child_process_count: &io::Result<usize>,
) -> bool {
    confirm
        || match child_process_count {
            Ok(count) => warn.should_warn(*count),
            Err(_) => warn != WarnBeforeTerminatingShell::Disabled,
        }
}
