use nmt_i18n::i18n;

use crate::remote::reconcile as reconcile_remote_session;
use crate::ui::settings::*;

pub(super) fn remote_session_page() -> SettingPage {
    SettingPage::new(i18n("settings-remote-title"))
        .default_open(true)
        .description(i18n("settings-remote-description"))
        .group(
            SettingGroup::new()
                .title(i18n("settings-remote-host-service"))
                .item(
                    SettingItem::new(
                        i18n("settings-remote-enable-host"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().remote_host_enabled,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_host_enabled = value;
                                reconcile_remote_host(cx);
                            },
                        ),
                    )
                    .description(i18n("settings-remote-enable-host-description")),
                )
                .item(
                    SettingItem::new(
                        i18n("settings-remote-relay-url"),
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_relay_url.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_relay_url = value;
                            },
                        ),
                    )
                    .description(i18n("settings-remote-relay-url-description")),
                )
                .item(
                    SettingItem::new(
                        i18n("settings-remote-access-token"),
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_access_token.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_access_token = value;
                            },
                        ),
                    )
                    .description(i18n("settings-remote-access-token-description")),
                ),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-remote-pairing-devices"))
                .item(SettingItem::render(|_, _, cx| remote_host_status(cx))),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-remote-connect-host"))
                .description(i18n("settings-remote-connect-host-description"))
                .item(
                    SettingItem::new(
                        i18n("settings-remote-pairing-code"),
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_pairing_input.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_pairing_input = value;
                            },
                        ),
                    )
                    .description(i18n("settings-remote-pairing-code-description")),
                )
                .item(SettingItem::render(|_, _, cx| remote_client_status(cx))),
        )
}

/// Start/stop/restart the background host service to match the live settings.
/// Called on discrete events (enable toggle, dialog close), never per keystroke.
#[cfg(windows)]
pub(crate) fn reconcile_remote_host(cx: &App) {
    let settings = cx.global::<AppSettings>();
    reconcile_remote_session(&RemoteSessionConfig {
        host_enabled: settings.remote_host_enabled,
        relay_url: settings.remote_relay_url.to_string(),
        access_token: settings.remote_access_token.to_string(),
    });
}

#[cfg(not(windows))]
pub(crate) fn reconcile_remote_host(_cx: &App) {}

#[cfg(windows)]
fn remote_host_status(cx: &mut App) -> Div {
    use crate::remote;

    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let surface = cx.theme().tokens.secondary;

    if !remote::is_running() {
        return v_flex().child(
            div()
                .py_2()
                .text_color(muted)
                .child(i18n("settings-remote-host-disabled")),
        );
    }

    let host_id = remote::host_id().unwrap_or_default();
    let pairing = cx.global::<AppSettings>().remote_pairing_code.clone();
    let devices = remote::list_devices();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .gap_2()
                .child(i18n("settings-remote-host-id"))
                .child(
                    div()
                        .font_family("monospace")
                        .text_color(muted)
                        .child(host_id),
                ),
        )
        .child(
            h_flex()
                .justify_between()
                .child(i18n("settings-remote-pair-new-device"))
                .child(
                    Button::new("remote-generate-pairing")
                        .outline()
                        .label(i18n("settings-remote-generate-pairing-code"))
                        .on_click(|_, _, cx: &mut App| {
                            if let Some(code) = remote::begin_pairing() {
                                cx.global_mut::<AppSettings>().remote_pairing_code =
                                    Some(code.encode());
                            }
                        }),
                ),
        )
        .when_some(pairing, |this, code| {
            this.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(UI_RADIUS)
                    .border_1()
                    .border_color(border)
                    .bg(surface)
                    .child(
                        div()
                            .text_color(muted)
                            .child(i18n("settings-remote-pairing-code-instruction")),
                    )
                    .child(div().font_family("monospace").child(code.clone()))
                    .child(
                        Button::new("remote-copy-pairing")
                            .outline()
                            .label(i18n("settings-common-copy"))
                            .on_click(move |_, _, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                            }),
                    ),
            )
        })
        .child(
            div()
                .mt_2()
                .text_color(muted)
                .child(i18n("settings-remote-authorized-devices")),
        )
        .when(devices.is_empty(), |this| {
            this.child(
                div()
                    .py_2()
                    .text_color(muted)
                    .child(i18n("settings-remote-no-devices")),
            )
        })
        .children(devices.into_iter().enumerate().map(|(index, device)| {
            let key = device.public_key.clone();
            h_flex()
                .w_full()
                .py_2()
                .justify_between()
                .border_b_1()
                .border_color(border)
                .child(device.name)
                .child(
                    Button::new(("remote-revoke", index))
                        .outline()
                        .label(i18n("settings-remote-revoke"))
                        .on_click(move |_, _, cx: &mut App| {
                            remote::revoke_device(&key);
                            cx.refresh_windows();
                        }),
                )
        }))
}

#[cfg(not(windows))]
fn remote_host_status(_cx: &mut App) -> Div {
    v_flex().child(div().child(i18n("settings-remote-windows-only")))
}

#[cfg(windows)]
fn remote_client_status(cx: &mut App) -> Div {
    use crate::remote;

    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let status = cx.global::<AppSettings>().remote_client_status.clone();
    let hosts = remote::known_hosts();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .child(i18n("settings-remote-pair-with-code"))
                .child(
                    Button::new("remote-pair")
                        .outline()
                        .label(i18n("settings-remote-pair"))
                        .on_click(|_, _, cx: &mut App| {
                            let code = cx.global::<AppSettings>().remote_pairing_input.to_string();
                            if code.trim().is_empty() {
                                cx.global_mut::<AppSettings>().remote_client_status =
                                    Some(i18n("settings-remote-enter-code-first").to_owned());
                                return;
                            }
                            cx.global_mut::<AppSettings>().remote_client_status =
                                Some(i18n("settings-remote-pairing").to_owned());
                            // Pairing is a network round trip: running it inline
                            // would freeze the window until the relay answers or
                            // the attempt times out.
                            cx.spawn(async move |cx| {
                                let paired = cx
                                    .background_executor()
                                    .spawn(async move {
                                        remote::pair_with_code(
                                            &code,
                                            i18n("settings-remote-default-host-name"),
                                        )
                                    })
                                    .await;
                                cx.update_global(|settings: &mut AppSettings, _| {
                                    let message = match paired {
                                        Ok(host) => {
                                            settings.remote_pairing_input = SharedString::default();
                                            i18n("settings-remote-paired-success")
                                                .replace("{name}", &host.name)
                                                .replace("{id}", &host.host_id)
                                        }
                                        Err(e) => i18n("settings-remote-pairing-failed")
                                            .replace("{error}", &e.to_string()),
                                    };
                                    settings.remote_client_status = Some(message);
                                })
                            })
                            .detach();
                        }),
                ),
        )
        .when_some(status, |this, message| {
            this.child(div().text_color(muted).child(message))
        })
        .child(
            div()
                .mt_2()
                .text_color(muted)
                .child(i18n("settings-remote-paired-hosts")),
        )
        .when(hosts.is_empty(), |this| {
            this.child(
                div()
                    .py_2()
                    .text_color(muted)
                    .child(i18n("settings-remote-no-hosts")),
            )
        })
        .children(hosts.into_iter().enumerate().map(|(index, host)| {
            let host_id = host.host_id.clone();
            h_flex()
                .w_full()
                .py_2()
                .justify_between()
                .border_b_1()
                .border_color(border)
                .child(
                    v_flex().child(host.name.clone()).child(
                        div()
                            .font_family("monospace")
                            .text_color(muted)
                            .child(host.host_id.clone()),
                    ),
                )
                .child(
                    Button::new(("remote-forget", index))
                        .outline()
                        .label(i18n("settings-remote-forget"))
                        .on_click(move |_, _, cx: &mut App| {
                            remote::forget_host(&host_id);
                            cx.refresh_windows();
                        }),
                )
        }))
}

#[cfg(not(windows))]
fn remote_client_status(_cx: &mut App) -> Div {
    v_flex()
}
