#[cfg(any(windows, test))]
use anyhow::Result;
use gpui::SharedString;
#[cfg(any(windows, test))]
use gpui_component::setting::SettingField;
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage};
use nmt_config::system::{NewlineShortcut, WarnBeforeTerminatingShell};
#[cfg(windows)]
use nmt_platform::{
    is_shell_integration_registered, register_shell_integration, set_system_notification_enabled,
    system_notification_enabled, unregister_shell_integration,
};
use rust_i18n::t;
#[cfg(any(windows, test))]
use tracing::warn;

#[cfg(windows)]
use crate::PlatformHandle;
use crate::ui::settings::fields::{settings_choice, settings_switch};
#[cfg(target_os = "macos")]
use crate::ui::settings::macos_page::macos_group;
#[cfg(any(windows, test))]
use crate::ui::settings::state::AppSettings;

pub(super) fn system_page(shell_integration_mismatched: bool) -> SettingPage {
    let page = SettingPage::new(t!("settings-system-title"))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(t!("settings-system-session"))
                .item(SettingItem::new(
                    t!("settings-system-restore-session"),
                    settings_switch(
                        |config| config.system.restore_last_session_when_opening,
                        |settings, value| {
                            settings.edit_system(|section| {
                                section.restore_last_session_when_opening = value
                            });
                        },
                    ),
                ))
                .item(SettingItem::new(
                    t!("settings-system-confirm-closing"),
                    settings_switch(
                        |config| config.system.confirm_before_closing_workspace,
                        |settings, value| {
                            settings.edit_system(|section| {
                                section.confirm_before_closing_workspace = value
                            });
                        },
                    ),
                ))
                .item(SettingItem::new(
                    t!("settings-system-warn-terminate"),
                    settings_choice(
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
                        |config| config.system.warn_before_terminating_shell.into(),
                        |settings, value| {
                            settings.edit_system(|section| {
                                section.warn_before_terminating_shell = value.into()
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
                    settings_switch(
                        |config| config.system.open_in_best_workspace,
                        |settings, value| {
                            settings.edit_system(|section| section.open_in_best_workspace = value);
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
                    settings_switch(
                        |config| config.system.manage_subprocess_job,
                        |settings, value| {
                            settings.edit_system(|section| section.manage_subprocess_job = value);
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
                settings_choice(
                    vec![
                        ("ctrl-enter".into(), "Ctrl-Enter".into()),
                        ("shift-enter".into(), "Shift-Enter".into()),
                        ("off".into(), t!("settings-common-off").into()),
                    ],
                    |config| config.system.newline_shortcut.into(),
                    |settings, value| {
                        settings.edit_system(|section| section.newline_shortcut = value.into());
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
