use crate::remote::reconcile as reconcile_remote_session;
use crate::ui::settings::*;

pub(super) fn remote_session_page() -> SettingPage {
    SettingPage::new("Remote Session")
        .default_open(true)
        .description(
            "Reach this machine's terminal sessions from other computers through a relay. \
             Traffic is end-to-end encrypted; the relay only ever sees ciphertext.",
        )
        .group(
            SettingGroup::new()
                .title("Host Service")
                .item(
                    SettingItem::new(
                        "Enable Host Service",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().remote_host_enabled,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_host_enabled = value;
                                reconcile_remote_host(cx);
                            },
                        ),
                    )
                    .description(
                        "Register with the relay so paired devices can attach to sessions on \
                         this machine. Sessions keep running while no client is connected.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Relay URL",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_relay_url.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_relay_url = value;
                            },
                        ),
                    )
                    .description(
                        "WebSocket endpoint, e.g. wss://relay.example.com/ws. Applied when you \
                         toggle the service or close settings.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Access Token",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_access_token.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_access_token = value;
                            },
                        ),
                    )
                    .description("Shared secret the relay requires to register this host."),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Pairing & Devices")
                .item(SettingItem::render(|_, _, cx| remote_host_status(cx))),
        )
        .group(
            SettingGroup::new()
                .title("Connect to a Host")
                .description(
                    "Pair with another machine's host service using the code it shows, then \
                     open remote tabs with Ctrl+Shift+R.",
                )
                .item(
                    SettingItem::new(
                        "Pairing Code",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_pairing_input.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_pairing_input = value;
                            },
                        ),
                    )
                    .description("Paste the code from the host machine, then click Pair."),
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
                .child("Enable the host service (with a relay URL and token) to pair devices."),
        );
    }

    let host_id = remote::host_id().unwrap_or_default();
    let pairing = cx.global::<AppSettings>().remote_pairing_code.clone();
    let devices = remote::list_devices();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex().gap_2().child("Host ID").child(
                div()
                    .font_family("monospace")
                    .text_color(muted)
                    .child(host_id),
            ),
        )
        .child(
            h_flex().justify_between().child("Pair a new device").child(
                Button::new("remote-generate-pairing")
                    .outline()
                    .label("Generate Pairing Code")
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
                            .child("Enter this code on the other computer within 5 minutes:"),
                    )
                    .child(div().font_family("monospace").child(code.clone()))
                    .child(
                        Button::new("remote-copy-pairing")
                            .outline()
                            .label("Copy")
                            .on_click(move |_, _, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                            }),
                    ),
            )
        })
        .child(div().mt_2().text_color(muted).child("Authorized Devices"))
        .when(devices.is_empty(), |this| {
            this.child(
                div()
                    .py_2()
                    .text_color(muted)
                    .child("No devices paired yet."),
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
                        .label("Revoke")
                        .on_click(move |_, _, cx: &mut App| {
                            remote::revoke_device(&key);
                            cx.refresh_windows();
                        }),
                )
        }))
}

#[cfg(not(windows))]
fn remote_host_status(_cx: &mut App) -> Div {
    v_flex().child(div().child("Remote sessions are only available on Windows."))
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
                .child("Pair with this code")
                .child(Button::new("remote-pair").outline().label("Pair").on_click(
                    |_, _, cx: &mut App| {
                        let code = cx.global::<AppSettings>().remote_pairing_input.to_string();
                        if code.trim().is_empty() {
                            cx.global_mut::<AppSettings>().remote_client_status =
                                Some("Enter a pairing code first.".to_owned());
                            return;
                        }
                        cx.global_mut::<AppSettings>().remote_client_status =
                            Some("Pairing…".to_owned());
                        // Pairing is a network round trip: running it inline
                        // would freeze the window until the relay answers or
                        // the attempt times out.
                        cx.spawn(async move |cx| {
                            let paired = cx
                                .background_executor()
                                .spawn(async move { remote::pair_with_code(&code, "remote host") })
                                .await;
                            cx.update_global(|settings: &mut AppSettings, _| {
                                let message = match paired {
                                    Ok(host) => {
                                        settings.remote_pairing_input = SharedString::default();
                                        format!("Paired with {} ({}).", host.name, host.host_id)
                                    }
                                    Err(e) => format!("Pairing failed: {e}"),
                                };
                                settings.remote_client_status = Some(message);
                            })
                        })
                        .detach();
                    },
                )),
        )
        .when_some(status, |this, message| {
            this.child(div().text_color(muted).child(message))
        })
        .child(div().mt_2().text_color(muted).child("Paired Hosts"))
        .when(hosts.is_empty(), |this| {
            this.child(div().py_2().text_color(muted).child("No hosts paired yet."))
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
                        .label("Forget")
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
