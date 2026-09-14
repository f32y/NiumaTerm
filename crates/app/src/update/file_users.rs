use std::borrow::Cow;
use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{AnyWindowHandle, App, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{
    DIALOG_BUTTON_MIN_WIDTH, Dialog, DialogClose, DialogContent, DialogFooter,
};
use gpui_component::{ActiveTheme as _, WindowExt as _, v_flex};
use nmt_platform::windows::restart_manager::{AffectedApplication, ApplicationKind};
use rust_i18n::t;

use crate::update::{self, FileUsePrompt, FileUsePromptReason};

pub(crate) fn open_file_use_prompt(
    handle: AnyWindowHandle,
    prompt: FileUsePrompt,
    cx: &mut App,
) -> bool {
    handle
        .update(cx, move |_, window, cx| {
            build_file_use_prompt(window, prompt, cx)
        })
        .is_ok()
}

fn build_file_use_prompt(window: &mut Window, prompt: FileUsePrompt, cx: &mut App) {
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
        let message = message.clone();
        let footer = file_use_footer(prompt.reason);

        dialog
            .title(title.clone())
            .overlay_closable(false)
            .content(move |content, _, cx| {
                file_use_body(
                    content,
                    message.clone(),
                    &applications,
                    has_explorer,
                    &manual,
                    cx,
                )
            })
            .footer(footer)
    });
}

fn file_use_footer(reason: FileUsePromptReason) -> DialogFooter {
    let mut footer = DialogFooter::new();

    match reason {
        FileUsePromptReason::InUse | FileUsePromptReason::RemainingUsers => {
            footer = footer.child(
                Button::new("app-update-close-file-users")
                    .min_w(DIALOG_BUTTON_MIN_WIDTH)
                    .danger()
                    .label(t!("settings-about-file-use-close-update"))
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
                    .label(t!("settings-about-file-use-retry"))
                    .on_click(|_, window, cx| {
                        window.close_dialog(cx);

                        update::inspect_file_users(cx);
                    }),
            );
        }
        FileUsePromptReason::RebootRequired => {}
    }

    let continue_button = Button::new("app-update-continue-file-use")
        .min_w(DIALOG_BUTTON_MIN_WIDTH)
        .label(t!("settings-about-file-use-continue"))
        .on_click(|_, window, cx| {
            window.close_dialog(cx);

            update::continue_install(cx);
        });

    footer = footer.child(
        if matches!(
            reason,
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
                .label(t!("settings-about-file-use-cancel"))
                .on_click(|_, _, cx| update::cancel_install(cx)),
        ),
    );

    footer
}

fn file_use_body(
    content: DialogContent,
    message: Cow<'static, str>,
    applications: &[String],
    has_explorer: bool,
    manual: &[String],
    cx: &App,
) -> DialogContent {
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
        body = body.child(t!("settings-about-file-use-explorer-warning"));
    }

    if !manual.is_empty() {
        body = body.child(
            t!(
                "settings-about-file-use-not-restartable",
                applications = &manual.join(", ")
            )
            .into_owned(),
        );
    }

    content.child(body)
}

pub(crate) fn open_recovery_warning(
    handle: AnyWindowHandle,
    applications: Vec<String>,
    cx: &mut App,
) -> bool {
    handle
        .update(cx, move |_, window, cx| {
            window.open_dialog(cx, move |dialog, _, _| {
                build_recovery_dialog(dialog, &applications)
            });
        })
        .is_ok()
}

fn build_recovery_dialog(dialog: Dialog, applications: &[String]) -> Dialog {
    let message = t!(
        "settings-about-recovery-warning-message",
        applications = &applications.join(", ")
    )
    .into_owned();

    dialog
        .title(t!("settings-about-recovery-warning-title"))
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
                    .label(t!("settings-about-recovery-restart"))
                    .on_click(|_, window, cx| {
                        window.close_dialog(cx);

                        update::complete_relaunch(cx);
                    }),
            ),
        )
}

fn prompt_title(reason: FileUsePromptReason) -> Cow<'static, str> {
    match reason {
        FileUsePromptReason::InUse => t!("settings-about-file-use-title"),
        FileUsePromptReason::CheckFailed => t!("settings-about-file-use-check-failed-title"),
        FileUsePromptReason::RebootRequired => t!("settings-about-file-use-reboot-title"),
        FileUsePromptReason::RemainingUsers => t!("settings-about-file-use-remaining-title"),
    }
}

fn prompt_message(reason: FileUsePromptReason) -> Cow<'static, str> {
    match reason {
        FileUsePromptReason::InUse => t!("settings-about-file-use-message"),
        FileUsePromptReason::CheckFailed => t!("settings-about-file-use-check-failed-message"),
        FileUsePromptReason::RebootRequired => t!("settings-about-file-use-reboot-message"),
        FileUsePromptReason::RemainingUsers => t!("settings-about-file-use-remaining-message"),
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
                t!(
                    "settings-about-file-use-unknown",
                    pid = application.process_id
                )
                .into_owned()
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
