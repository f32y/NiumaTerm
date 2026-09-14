#[cfg(any(windows, test))]
use anyhow::Result;
use rust_i18n::t;

#[cfg(target_os = "macos")]
use crate::ui::settings::macos_page::macos_group;
use crate::ui::settings::*;

pub(super) fn system_page(shell_integration_mismatched: bool) -> SettingPage {
    let page = SettingPage::new(t!("settings-system-title"))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(t!("settings-system-session"))
                .item(SettingItem::new(
                    t!("settings-system-restore-session"),
                    SettingField::switch(
                        |cx| {
                            cx.global::<AppSettings>()
                                .config()
                                .system
                                .restore_last_session_when_opening
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>().edit_system(|section| {
                                section.restore_last_session_when_opening = value
                            });
                        },
                    ),
                ))
                .item(SettingItem::new(
                    t!("settings-system-confirm-closing"),
                    SettingField::switch(
                        |cx| {
                            cx.global::<AppSettings>()
                                .config()
                                .system
                                .confirm_before_closing_workspace
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>().edit_system(|section| {
                                section.confirm_before_closing_workspace = value
                            });
                        },
                    ),
                ))
                .item(SettingItem::new(
                    t!("settings-system-warn-terminate"),
                    SettingField::dropdown(
                        vec![
                            (
                                "disabled".into(),
                                t!("settings-system-warn-disabled").into(),
                            ),
                            (
                                "when-child-processes-running".into(),
                                t!("settings-system-warn-when-children").into(),
                            ),
                            ("always".into(), t!("settings-system-warn-always").into()),
                        ],
                        |cx| {
                            let key: &str = cx
                                .global::<AppSettings>()
                                .config()
                                .system
                                .warn_before_terminating_shell
                                .into();

                            SharedString::from(key)
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>().edit_system(|section| {
                                section.warn_before_terminating_shell = value.as_str().into()
                            });
                        },
                    )
                    .default_value(SharedString::from(<&str>::from(
                        WarnBeforeTerminatingShell::WhenChildProcessesRunning,
                    ))),
                )),
        );

    let page = page.group(
        SettingGroup::new()
            .title(t!("settings-system-workspaces"))
            .item(
                SettingItem::new(
                    t!("settings-system-open-best-workspace"),
                    SettingField::switch(
                        |cx| {
                            cx.global::<AppSettings>()
                                .config()
                                .system
                                .open_in_best_workspace
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>()
                                .edit_system(|section| section.open_in_best_workspace = value);
                        },
                    ),
                )
                .description(t!("settings-system-open-best-workspace-description").into_owned()),
            ),
    );

    #[cfg(windows)]
    let page = page.group(
        SettingGroup::new()
            .title(t!("settings-system-integration"))
            .item(SettingItem::new(
                if shell_integration_mismatched {
                    t!("settings-system-context-menu-warning")
                } else {
                    t!("settings-system-context-menu")
                },
                SettingField::switch(
                    |_| is_shell_integration_registered(),
                    |value, _| {
                        let result = if value {
                            register_shell_integration()
                        } else {
                            unregister_shell_integration()
                        };

                        if let Err(err) = result {
                            warn!("failed to toggle Windows context menu: {err:#}");
                        }
                    },
                ),
            ))
            .item(SettingItem::new(
                t!("settings-system-notification"),
                windows_notification_field(
                    system_notification_enabled,
                    set_system_notification_enabled,
                ),
            ))
            .item(
                SettingItem::new(
                    t!("settings-system-manage-job"),
                    SettingField::switch(
                        |cx| {
                            cx.global::<AppSettings>()
                                .config()
                                .system
                                .manage_subprocess_job
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>()
                                .edit_system(|section| section.manage_subprocess_job = value);
                        },
                    ),
                )
                .description(t!("settings-system-manage-job-description").into_owned()),
            ),
    );

    #[cfg(target_os = "macos")]
    let page = page.group(macos_group());

    #[cfg(not(windows))]
    let _ = shell_integration_mismatched;

    #[cfg(windows)]
    let page = page.group(
        SettingGroup::new()
            .title(t!("settings-system-performance"))
            .item(SettingItem::new(
                t!("settings-system-prioritize-ui"),
                SettingField::switch(
                    |cx| {
                        cx.global::<AppSettings>()
                            .config()
                            .system
                            .prioritize_ui_threads
                    },
                    |value, cx| {
                        cx.global_mut::<AppSettings>()
                            .edit_system(|section| section.prioritize_ui_threads = value);

                        #[cfg(windows)]
                        cx.global::<PlatformHandle>()
                            .0
                            .set_ui_thread_priority(value);
                    },
                ),
            )),
    );

    page.group(
        SettingGroup::new()
            .title(t!("settings-system-input"))
            .item(SettingItem::new(
                t!("settings-system-newline-shortcut"),
                SettingField::dropdown(
                    vec![
                        ("ctrl-enter".into(), "Ctrl-Enter".into()),
                        ("shift-enter".into(), "Shift-Enter".into()),
                        ("off".into(), t!("settings-common-off").into()),
                    ],
                    |cx| {
                        let key: &str = cx
                            .global::<AppSettings>()
                            .config()
                            .system
                            .newline_shortcut
                            .into();

                        SharedString::from(key)
                    },
                    |value, cx| {
                        cx.global_mut::<AppSettings>().edit_system(|section| {
                            section.newline_shortcut = value.as_str().into()
                        });
                    },
                )
                .default_value(SharedString::from(<&str>::from(NewlineShortcut::CtrlEnter))),
            )),
    )
}

#[cfg(any(windows, test))]
pub(super) fn windows_notification_field(
    registered: impl Fn() -> bool + 'static,
    set_registered: impl Fn(bool) -> Result<()> + 'static,
) -> SettingField<bool> {
    SettingField::switch(
        move |cx| {
            cx.global::<AppSettings>()
                .config()
                .system
                .send_system_notifications
                && registered()
        },
        move |value, cx| match set_registered(value) {
            Ok(()) => {
                // Imported settings can disable delivery while Windows remains
                // registered. Update both gates only after registration succeeds.
                cx.global_mut::<AppSettings>()
                    .edit_system(|section| section.send_system_notifications = value);
            }
            Err(err) => warn!("failed to toggle system notifications: {err:#}"),
        },
    )
}
