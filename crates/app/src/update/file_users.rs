use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{AnyWindowHandle, App, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DIALOG_BUTTON_MIN_WIDTH, DialogClose, DialogFooter};
use gpui_component::{ActiveTheme as _, WindowExt as _, v_flex};
use nmt_i18n::i18n;
use nmt_platform::windows::restart_manager::{AffectedApplication, ApplicationKind};

use crate::update::{self, FileUsePrompt, FileUsePromptReason};

pub(crate) fn open_file_use_prompt(
    handle: AnyWindowHandle,
    prompt: FileUsePrompt,
    cx: &mut App,
) -> bool {
    handle
        .update(cx, move |_, window, cx| {
            let title = prompt_title(prompt.reason);
            let message = prompt_message(prompt.reason);
            let applications = display_names(&prompt.applications);
            let has_explorer = prompt
                .applications
                .iter()
                .any(|application| application.kind == ApplicationKind::Explorer);
            let manual = display_names(
                &prompt
                    .applications
                    .iter()
                    .filter(|application| !application.restartable)
                    .cloned()
                    .collect::<Vec<_>>(),
            );

            window.open_dialog(cx, move |dialog, _, _| {
                let applications = applications.clone();
                let manual = manual.clone();
                let mut footer = DialogFooter::new();

                match prompt.reason {
                    FileUsePromptReason::InUse | FileUsePromptReason::RemainingUsers => {
                        footer = footer.child(
                            Button::new("app-update-close-file-users")
                                .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                .danger()
                                .label(i18n("settings-about-file-use-close-update"))
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                    update::close_file_users(cx);
                                }),
                        );
                    }
                    FileUsePromptReason::CheckFailed => {
                        footer = footer.child(
                            Button::new("app-update-retry-file-use")
                                .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                .primary()
                                .label(i18n("settings-about-file-use-retry"))
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                    update::retry_file_use(cx);
                                }),
                        );
                    }
                    FileUsePromptReason::RebootRequired => {}
                }

                let continue_button = Button::new("app-update-continue-file-use")
                    .min_w(DIALOG_BUTTON_MIN_WIDTH)
                    .label(i18n("settings-about-file-use-continue"))
                    .on_click(|_, window, cx| {
                        window.close_dialog(cx);
                        update::continue_install(cx);
                    });
                footer = footer.child(
                    if matches!(
                        prompt.reason,
                        FileUsePromptReason::InUse | FileUsePromptReason::RemainingUsers
                    ) {
                        continue_button.outline()
                    } else {
                        continue_button.primary()
                    },
                );
                footer = footer.child(
                    DialogClose::new().child(
                        Button::new("app-update-cancel-file-use")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .label(i18n("settings-about-file-use-cancel"))
                            .on_click(|_, _, cx| update::cancel_install(cx)),
                    ),
                );

                dialog
                    .title(title)
                    .overlay_closable(false)
                    .content(move |content, _, cx| {
                        let mut body = v_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(message);
                        if !applications.is_empty() {
                            body = body.child(
                                v_flex().gap_1().children(
                                    applications
                                        .iter()
                                        .map(|application| div().child(application.clone())),
                                ),
                            );
                        }
                        if has_explorer {
                            body = body.child(i18n("settings-about-file-use-explorer-warning"));
                        }
                        if !manual.is_empty() {
                            body = body.child(
                                i18n("settings-about-file-use-not-restartable")
                                    .replace("{applications}", &manual.join(", ")),
                            );
                        }
                        content.child(body)
                    })
                    .footer(footer)
            });
        })
        .is_ok()
}

pub(crate) fn open_recovery_warning(
    handle: AnyWindowHandle,
    applications: Vec<String>,
    cx: &mut App,
) -> bool {
    handle
        .update(cx, move |_, window, cx| {
            window.open_dialog(cx, move |dialog, _, _| {
                let message = i18n("settings-about-recovery-warning-message")
                    .replace("{applications}", &applications.join(", "));
                dialog
                    .title(i18n("settings-about-recovery-warning-title"))
                    .overlay_closable(false)
                    .content(move |content, _, cx| {
                        content.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(message.clone()),
                        )
                    })
                    .footer(
                        DialogFooter::new().child(
                            Button::new("app-update-finish-relaunch")
                                .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                .primary()
                                .label(i18n("settings-about-recovery-restart"))
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                    update::complete_relaunch(cx);
                                }),
                        ),
                    )
            });
        })
        .is_ok()
}

fn prompt_title(reason: FileUsePromptReason) -> &'static str {
    match reason {
        FileUsePromptReason::InUse => i18n("settings-about-file-use-title"),
        FileUsePromptReason::CheckFailed => i18n("settings-about-file-use-check-failed-title"),
        FileUsePromptReason::RebootRequired => i18n("settings-about-file-use-reboot-title"),
        FileUsePromptReason::RemainingUsers => i18n("settings-about-file-use-remaining-title"),
    }
}

fn prompt_message(reason: FileUsePromptReason) -> &'static str {
    match reason {
        FileUsePromptReason::InUse => i18n("settings-about-file-use-message"),
        FileUsePromptReason::CheckFailed => i18n("settings-about-file-use-check-failed-message"),
        FileUsePromptReason::RebootRequired => i18n("settings-about-file-use-reboot-message"),
        FileUsePromptReason::RemainingUsers => i18n("settings-about-file-use-remaining-message"),
    }
}

pub(super) fn display_names(applications: &[AffectedApplication]) -> Vec<String> {
    let mut counts = HashMap::new();
    for application in applications {
        *counts.entry(application.name.as_str()).or_insert(0usize) += 1;
    }

    applications
        .iter()
        .map(|application| {
            let name = if application.name.is_empty() {
                i18n("settings-about-file-use-unknown")
                    .replace("{pid}", &application.process_id.to_string())
            } else {
                application.name.clone()
            };
            if counts.get(application.name.as_str()).copied().unwrap_or(0) > 1 {
                format!("{name} (PID {})", application.process_id)
            } else {
                name
            }
        })
        .collect()
}
