#[cfg(any(windows, test))]
use anyhow::Result;
use gpui::{
    App, AppContext as _, Entity, ParentElement as _, SharedString, Styled as _, Subscription,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_component::{Disableable as _, Sizable as _, h_flex};
use nmt_config::system::{NewlineShortcut, ProxyMode, WarnBeforeTerminatingShell};
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
use crate::ui::settings::state::AppSettings;

/// How the `ctrl-enter` newline shortcut is pressed on this platform. macOS
/// text inputs read Command-Enter as the secondary Enter, and terminal tabs
/// follow them there, so the choice is labeled with the key that works.
#[cfg(target_os = "macos")]
const SECONDARY_ENTER_LABEL: &str = "Cmd-Enter";

#[cfg(not(target_os = "macos"))]
const SECONDARY_ENTER_LABEL: &str = "Ctrl-Enter";

pub(super) fn system_page(shell_integration_mismatched: bool, proxy: ProxyMode) -> SettingPage {
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
            )),
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
                        ("ctrl-enter".into(), SECONDARY_ENTER_LABEL.into()),
                        ("shift-enter".into(), "Shift-Enter".into()),
                        ("off".into(), t!("settings-common-off").into()),
                    ],
                    |config| config.system.newline_shortcut.into(),
                    |settings, value| {
                        settings.edit_system(|section| section.newline_shortcut = value.into());
                    },
                )
                .default_value(SharedString::from(<&str>::from(NewlineShortcut::default()))),
            )),
    )
    .group(network_group(proxy))
}

fn network_group(proxy: ProxyMode) -> SettingGroup {
    // Only an explicit proxy reads the address. The field keeps its value
    // while disabled, so switching back restores the last address.
    let address_used = match proxy {
        ProxyMode::Off | ProxyMode::System => false,
        ProxyMode::Http | ProxyMode::Socks => true,
    };

    SettingGroup::new()
        .title(t!("settings-system-network"))
        .item(
            SettingItem::new(
                t!("settings-system-proxy"),
                settings_choice(
                    vec![
                        ("off".into(), t!("settings-common-off").into()),
                        ("system".into(), t!("settings-system-proxy-system").into()),
                        ("http".into(), "HTTP".into()),
                        ("socks".into(), "SOCKS5".into()),
                    ],
                    |config| config.system.proxy.into(),
                    |settings, value| {
                        settings.edit_system(|section| section.proxy = value.into());
                    },
                )
                .default_value(SharedString::from(<&str>::from(ProxyMode::System))),
            )
            .description(t!("settings-system-proxy-description").into_owned()),
        )
        .item(
            SettingItem::new(t!("settings-system-proxy-url"), proxy_url_field())
                .description(t!("settings-system-proxy-url-description").into_owned())
                .disabled(!address_used),
        )
}

struct ProxyUrlInput {
    input: Entity<InputState>,

    /// The saved address the draft was last reset to.
    saved: String,

    _subscription: Subscription,
}

/// The proxy address draft and its Apply button. Every saved setting change
/// reconnects through the new route, so a half-typed address is kept as a
/// draft until Apply or Enter. The button stays disabled while the draft
/// matches the saved address, which shows that nothing is pending.
fn proxy_url_field() -> SettingField<SharedString> {
    SettingField::render(|options, window, cx| {
        let saved = cx.global::<AppSettings>().config().system.proxy_url.clone();

        let state = window.use_keyed_state("system-proxy-url", cx, |window, cx| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("127.0.0.1:7890")
                    .default_value(saved.clone())
            });

            // Notifying the state re-renders the page, which recomputes
            // whether the button has anything to apply.
            let subscription =
                cx.subscribe(&input, |_, input, event: &InputEvent, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => apply_proxy_url(&input, cx),
                    _ => {}
                });

            ProxyUrlInput {
                input,
                saved: saved.clone(),
                _subscription: subscription,
            }
        });

        let input = state.read(cx).input.clone();

        // A saved address changed elsewhere, by a reset or an imported
        // config, replaces the draft so the field never shows a stale one.
        if state.read(cx).saved != saved {
            input.update(cx, |input, cx| input.set_value(saved.clone(), window, cx));
            state.update(cx, |state, _| state.saved = saved.clone());
        }

        let pending = input.read(cx).value().trim() != saved;

        h_flex()
            .gap_2()
            .child(
                Input::new(&input)
                    .disabled(options.is_disabled())
                    .with_size(options.size())
                    .w_64(),
            )
            .child(
                Button::new("system-proxy-apply")
                    .outline()
                    .label(t!("settings-system-proxy-apply"))
                    .with_size(options.size())
                    .disabled(options.is_disabled() || !pending)
                    .on_click(move |_, _, cx: &mut App| apply_proxy_url(&input, cx)),
            )
    })
}

fn apply_proxy_url(input: &Entity<InputState>, cx: &mut App) {
    let url = input.read(cx).value().trim().to_owned();

    cx.global_mut::<AppSettings>()
        .edit_system(|section| section.proxy_url = url);
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
