use rust_i18n::t;

use crate::ui::settings::*;

pub(super) fn system_page(shell_integration_mismatched: bool) -> SettingPage {
    let when_child_processes_running_key: &str =
        WarnBeforeTerminatingShell::WhenChildProcessesRunning.into();

    let ctrl_enter_key: &str = NewlineShortcut::CtrlEnter.into();

    SettingPage::new(t!("settings-system-title"))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(t!("settings-system-session"))
                .item(SettingItem::new(
                    t!("settings-system-restore-session"),
                    SettingField::switch(
                        |cx| {
                            cx.global::<AppSettings>()
                                .system
                                .restore_last_session_when_opening
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>()
                                .system
                                .restore_last_session_when_opening = value;
                        },
                    ),
                ))
                .item(SettingItem::new(
                    t!("settings-system-confirm-closing"),
                    SettingField::switch(
                        |cx| {
                            cx.global::<AppSettings>()
                                .system
                                .confirm_before_closing_workspace
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>()
                                .system
                                .confirm_before_closing_workspace = value;
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
                                .system
                                .warn_before_terminating_shell
                                .into();

                            key.into()
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>()
                                .system
                                .warn_before_terminating_shell = value.as_str().into();
                        },
                    )
                    .default_value(when_child_processes_running_key),
                )),
        )
        .group(
            SettingGroup::new()
                .title(t!("settings-system-windows"))
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
                    SettingField::switch(
                        |_| system_notification_enabled(),
                        |value, _| {
                            if let Err(err) = set_system_notification_enabled(value) {
                                warn!("failed to toggle system notifications: {err:#}");
                            }
                        },
                    ),
                ))
                .item(
                    SettingItem::new(
                        t!("settings-system-open-best-workspace"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().system.open_in_best_workspace,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().system.open_in_best_workspace =
                                    value;
                            },
                        ),
                    )
                    .description(
                        t!("settings-system-open-best-workspace-description").into_owned(),
                    ),
                )
                .item(
                    SettingItem::new(
                        t!("settings-system-manage-job"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().system.manage_subprocess_job,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().system.manage_subprocess_job = value;
                            },
                        ),
                    )
                    .description(t!("settings-system-manage-job-description").into_owned()),
                ),
        )
        .group(
            SettingGroup::new()
                .title(t!("settings-system-performance"))
                .item(SettingItem::new(
                    t!("settings-system-prioritize-ui"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().system.prioritize_ui_threads,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().system.prioritize_ui_threads = value;

                            #[cfg(windows)]
                            cx.global::<PlatformHandle>()
                                .0
                                .set_ui_thread_priority(value);
                        },
                    ),
                )),
        )
        .group(
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
                            let key: &str =
                                cx.global::<AppSettings>().system.newline_shortcut.into();

                            key.into()
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>().system.newline_shortcut =
                                value.as_str().into();
                        },
                    )
                    .default_value(ctrl_enter_key),
                )),
        )
}
