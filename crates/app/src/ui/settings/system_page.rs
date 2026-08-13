use nmt_i18n::i18n;

use crate::ui::settings::*;

pub(super) fn system_page(shell_integration_mismatched: bool) -> SettingPage {
    SettingPage::new(i18n("settings-system-title"))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(i18n("settings-system-session"))
                .item(SettingItem::new(
                    i18n("settings-system-restore-session"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().restore_last_session_when_opening,
                        |value, cx| {
                            cx.global_mut::<AppSettings>()
                                .restore_last_session_when_opening = value;
                        },
                    ),
                ))
                .item(SettingItem::new(
                    i18n("settings-system-confirm-closing"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().confirm_before_closing,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().confirm_before_closing = value;
                        },
                    ),
                ))
                .item(SettingItem::new(
                    i18n("settings-system-warn-terminate"),
                    SettingField::dropdown(
                        vec![
                            (
                                "disabled".into(),
                                i18n("settings-system-warn-disabled").into(),
                            ),
                            (
                                "when-child-processes-running".into(),
                                i18n("settings-system-warn-when-children").into(),
                            ),
                            ("always".into(), i18n("settings-system-warn-always").into()),
                        ],
                        |cx| {
                            cx.global::<AppSettings>()
                                .warn_before_terminating_shell
                                .as_str()
                                .into()
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>().warn_before_terminating_shell =
                                WarnBeforeTerminatingShell::from_value(&value);
                        },
                    )
                    .default_value(SharedString::from(
                        WarnBeforeTerminatingShell::WhenChildProcessesRunning.as_str(),
                    )),
                )),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-system-process"))
                .item(
                    SettingItem::new(
                        i18n("settings-system-manage-job"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().manage_subprocess_job,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().manage_subprocess_job = value;
                            },
                        ),
                    )
                    .description(i18n("settings-system-manage-job-description")),
                ),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-system-windows"))
                .item(SettingItem::new(
                    if shell_integration_mismatched {
                        i18n("settings-system-context-menu-warning")
                    } else {
                        i18n("settings-system-context-menu")
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
                    i18n("settings-system-notification"),
                    SettingField::switch(
                        |_| system_notification_enabled(),
                        |value, _| {
                            if let Err(err) = set_system_notification_enabled(value) {
                                warn!("failed to toggle system notifications: {err:#}");
                            }
                        },
                    ),
                )),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-system-performance"))
                .item(SettingItem::new(
                    i18n("settings-system-prioritize-ui"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().prioritize_ui_threads,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().prioritize_ui_threads = value;

                            cx.global::<PlatformHandle>()
                                .0
                                .set_ui_thread_priority(value);
                        },
                    ),
                )),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-system-input"))
                .item(SettingItem::new(
                    i18n("settings-system-newline-shortcut"),
                    SettingField::dropdown(
                        vec![
                            ("ctrl-enter".into(), "Ctrl-Enter".into()),
                            ("shift-enter".into(), "Shift-Enter".into()),
                            ("off".into(), i18n("settings-common-off").into()),
                        ],
                        |cx| cx.global::<AppSettings>().newline_shortcut.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().newline_shortcut =
                                NewlineShortcut::from_value(&value);
                        },
                    )
                    .default_value(SharedString::from(NewlineShortcut::CtrlEnter.as_str())),
                )),
        )
}
