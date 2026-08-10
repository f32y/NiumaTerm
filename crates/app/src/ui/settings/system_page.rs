use crate::ui::settings::*;

pub(super) fn system_page(shell_integration_mismatched: bool) -> SettingPage {
    SettingPage::new("System")
                    .default_open(true)
                    .group(
                        SettingGroup::new().title("Session").item(
                            SettingItem::new(
                                "Restore last session when opening",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().restore_last_session_when_opening,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>()
                                            .restore_last_session_when_opening = value;
                                    },
                                ),
                            )
                            .description("Reopen saved workspaces and tabs on startup."),
                        ),
                    )
                    .group(
                        SettingGroup::new().title("Workspace").item(
                            SettingItem::new(
                                "Confirm before closing",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().confirm_before_closing,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().confirm_before_closing = value;
                                    },
                                ),
                            )
                            .description(
                                "Ask for confirmation when closing a workspace, Agent tab, or window.",
                            ),
                        ),
                    )
                    .group(
                        SettingGroup::new()
                            .title("Process")
                            .item(
                                SettingItem::new(
                                    "Manage subprocess by Windows Job API",
                                    SettingField::switch(
                                        |cx| cx.global::<AppSettings>().manage_subprocess_job,
                                        |value, cx| {
                                            cx.global_mut::<AppSettings>().manage_subprocess_job =
                                                value;
                                        },
                                    ),
                                )
                                .description(
                                    "Closing a tab kills the shell's entire process tree. \
                             Applies to newly opened tabs.",
                                ),
                            )
                            .item(
                                SettingItem::new(
                                    "Warn before terminating shell",
                                    SettingField::dropdown(
                                        vec![
                                            ("disabled".into(), "Disabled".into()),
                                            (
                                                "when-child-processes-running".into(),
                                                "When child processes running".into(),
                                            ),
                                            ("always".into(), "Always".into()),
                                        ],
                                        |cx| {
                                            cx.global::<AppSettings>()
                                                .warn_before_terminating_shell
                                                .as_str()
                                                .into()
                                        },
                                        |value, cx| {
                                            cx.global_mut::<AppSettings>()
                                                .warn_before_terminating_shell =
                                                WarnBeforeTerminatingShell::from_value(&value);
                                        },
                                    )
                                    .default_value(SharedString::from(
                                        WarnBeforeTerminatingShell::WhenChildProcessesRunning.as_str(),
                                    )),
                                )
                                .description(
                                    "Choose when closing a shell asks for confirmation. Detecting \
                             child processes requires Job management.",
                                ),
                            ),
                    )
                    .group(
                        SettingGroup::new()
                            .title("Windows")
                            .item(
                                SettingItem::new(
                                    if shell_integration_mismatched {
                                        "Enable Windows Context Menu  ⚠"
                                    } else {
                                        "Enable Windows Context Menu"
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
                                                warn!(
                                                    "failed to toggle Windows context menu: {err:#}"
                                                );
                                            }
                                        },
                                    ),
                                )
                                .description(if shell_integration_mismatched {
                                    "The registered shell extension does not point to the DLL beside the current NiumaTerm executable."
                                } else {
                                    "Add NiumaTerm actions to File Explorer directory menus."
                                }),
                            )
                            .item(
                                SettingItem::new(
                                    "Enable System Notification",
                                    SettingField::switch(
                                        |_| system_notification_enabled(),
                                        |value, _| {
                                            if let Err(err) =
                                                set_system_notification_enabled(value)
                                            {
                                                warn!(
                                                    "failed to toggle system notifications: {err:#}"
                                                );
                                            }
                                        },
                                    ),
                                )
                                .description(
                                    "Show Windows notifications for terminal and agent events.",
                                ),
                            ),
                    )
                    .group(
                        SettingGroup::new().title("Performance").item(
                            SettingItem::new(
                                "Prioritize UI threads",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().prioritize_ui_threads,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().prioritize_ui_threads = value;

                                        cx.global::<PlatformHandle>()
                                            .0
                                            .set_ui_thread_priority(value);
                                    },
                                ),
                            )
                            .description("Raise the main and render thread priority to AboveNormal."),
                        ),
                    )
}
