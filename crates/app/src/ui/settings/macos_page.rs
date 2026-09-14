use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{Context, Entity, IntoElement, Render, Window};
use gpui_component::v_flex;
use nmt_platform::macos_notifications::{
    NotificationPermission, notification_permission, request_notification_permission,
};
use rust_i18n::t;

use crate::ui::settings::*;

pub(super) fn macos_group() -> SettingGroup {
    SettingGroup::new()
        .title(t!("settings-system-integration"))
        .item(
            SettingItem::new(
                t!("settings-system-send-notifications"),
                SettingField::switch(
                    |cx| {
                        cx.global::<AppSettings>()
                            .config()
                            .system
                            .send_system_notifications
                    },
                    |value, cx| {
                        cx.global_mut::<AppSettings>()
                            .edit_system(|section| section.send_system_notifications = value)
                    },
                ),
            )
            .description(t!("settings-system-send-notifications-description").into_owned()),
        )
        .item(
            SettingItem::new(
                t!("settings-system-notification-permission"),
                SettingField::render(|_, window, cx| {
                    let state: Entity<PermissionView> =
                        window.use_keyed_state("macos-notification-permission", cx, |_, cx| {
                            let mut view = PermissionView {
                                status: None,
                                pending: false,
                            };

                            view.refresh(cx);

                            view
                        });

                    state
                }),
            )
            .description(t!("settings-system-notification-permission-description").into_owned()),
        )
}

struct PermissionView {
    status: Option<NotificationPermission>,
    pending: bool,
}

impl PermissionView {
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (sender, receiver) = mpsc::unbounded();

        notification_permission(move |status| {
            let _ = sender.unbounded_send(status);
        });

        self.receive(receiver, cx);
    }

    fn request(&mut self, cx: &mut Context<Self>) {
        let (sender, receiver) = mpsc::unbounded();

        request_notification_permission(move |status| {
            let _ = sender.unbounded_send(status);
        });

        self.receive(receiver, cx);
    }

    fn receive(
        &mut self,
        mut receiver: mpsc::UnboundedReceiver<NotificationPermission>,
        cx: &mut Context<Self>,
    ) {
        self.pending = true;

        cx.notify();

        cx.spawn(async move |this, cx| {
            let status = receiver.next().await;

            let _ = this.update(cx, |this, cx| {
                this.status = status;
                this.pending = false;

                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for PermissionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let key = match self.status {
            None => "settings-system-permission-loading",
            Some(NotificationPermission::NotDetermined) => {
                "settings-system-permission-not-determined"
            }
            Some(NotificationPermission::Denied) => "settings-system-permission-denied",
            Some(NotificationPermission::Authorized) => "settings-system-permission-authorized",
            Some(NotificationPermission::Provisional) => "settings-system-permission-provisional",
            Some(NotificationPermission::Unavailable) => "settings-system-permission-unavailable",
        };

        v_flex()
            .items_end()
            .gap_2()
            .child(Label::new(t!(key)).text_sm())
            .child(
                Button::new("refresh-notification-permission")
                    .outline()
                    .label(t!("settings-system-permission-refresh"))
                    .disabled(self.pending)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .when(
                self.status == Some(NotificationPermission::NotDetermined),
                |row| {
                    row.child(
                        Button::new("request-notification-permission")
                            .outline()
                            .label(t!("settings-system-permission-request"))
                            .disabled(self.pending)
                            .on_click(cx.listener(|this, _, _, cx| this.request(cx))),
                    )
                },
            )
    }
}
