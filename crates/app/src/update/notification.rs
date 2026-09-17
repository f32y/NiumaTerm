#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;

use gpui::{App, AppContext as _, Entity, Global};
use gpui_component::IconName;
use gpui_component::notification::Notification;
use nmt_updater::windows::{InstallError, Status};
use rust_i18n::{locale, t};

use crate::ui::notification_card::{
    NotificationAction, NotificationCard, NotificationProgress, NotificationTone,
};
use crate::update;

/// Each window owns a card; dismissal is shared so switching windows cannot
/// bring back an update the user already set aside.
#[derive(Default)]
pub(crate) struct UpdateNotification {
    card: Option<UpdateCard>,
}

struct UpdateCard {
    view: UpdateNotice,
    locale: String,
    entity: Entity<Notification>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum UpdateNotice {
    Available { version: String, url: String },
    Installing(String),
    Failed(InstallError),
}

#[derive(Default)]
struct DismissedUpdate(Option<UpdateNotice>);

impl Global for DismissedUpdate {}

impl UpdateNotification {
    pub(crate) fn render(&mut self, cx: &mut App) -> Option<Entity<Notification>> {
        let view = match update::status(cx) {
            Status::Available(release) => Some(UpdateNotice::Available {
                version: release.label,
                url: release.page_url,
            }),
            Status::Installing(release)
            | Status::InspectingFileUse(release)
            | Status::AwaitingFileUse(release)
            | Status::ClosingFileUsers(release) => Some(UpdateNotice::Installing(release.label)),
            Status::InstallFailed(error) => Some(UpdateNotice::Failed(error)),
            _ => None,
        };

        self.sync(view, cx)
    }

    fn sync(&mut self, view: Option<UpdateNotice>, cx: &mut App) -> Option<Entity<Notification>> {
        let dismissed = cx
            .try_global::<DismissedUpdate>()
            .and_then(|state| state.0.as_ref());

        // A hidden progress or error card belongs to one attempt. A hidden
        // available version stays hidden through later checks of that version.
        if matches!(
            dismissed,
            Some(UpdateNotice::Installing(_) | UpdateNotice::Failed(_))
        ) && dismissed != view.as_ref()
        {
            cx.global_mut::<DismissedUpdate>().0 = None;
        }

        let visible = view.filter(|view| {
            cx.try_global::<DismissedUpdate>()
                .and_then(|state| state.0.as_ref())
                != Some(view)
        });

        let Some(view) = visible else {
            self.card = None;

            return None;
        };

        let language = locale().to_string();

        let card = self.card.get_or_insert_with(|| UpdateCard {
            entity: cx.new(|_| view.notification()),
            view: view.clone(),
            locale: language.clone(),
        });

        if card.view != view || card.locale != language {
            card.entity
                .update(cx, |card, _| *card = view.notification());

            card.view = view;
            card.locale = language;
        }

        Some(card.entity.clone())
    }
}

impl UpdateNotice {
    fn notification(&self) -> Notification {
        let card = match self {
            Self::Available { version, url } => {
                let release_url = url.clone();
                let install_url = url.clone();

                NotificationCard {
                    title: t!("app-update-notice-available-title").into(),
                    message: format!("{} → {version}", env!("NIUMATERM_VERSION")).into(),
                    icon: Some(IconName::ArrowDown.into()),
                    primary: Some(NotificationAction::new(
                        t!("settings-about-install-button"),
                        move |window, cx| {
                            if let Status::Available(release) = update::status(cx)
                                && release.page_url == install_url
                            {
                                update::install_now(window, cx);
                            }
                        },
                    )),
                    secondary: Some(NotificationAction::new(
                        t!("settings-about-open-release"),
                        move |_, cx| cx.open_url(&release_url),
                    )),
                    ..NotificationCard::default()
                }
            }
            Self::Installing(version) => NotificationCard {
                title: t!("app-update-notice-installing-title").into(),
                message: t!("settings-about-installing", version = version).into(),
                progress: NotificationProgress::Indeterminate,
                ..NotificationCard::default()
            },
            Self::Failed(error) => NotificationCard {
                title: t!("app-update-notice-failed-title").into(),
                message: install_error_text(error).into(),
                tone: NotificationTone::Error,
                ..NotificationCard::default()
            },
        };

        let dismissed = self.clone();

        card.build()
            .id::<UpdateNotification>()
            .on_close(move |_, cx| {
                cx.set_global(DismissedUpdate(Some(dismissed.clone())));
                cx.refresh_windows();
            })
    }
}

pub(crate) fn install_error_text(error: &InstallError) -> String {
    match error {
        InstallError::NoPackage => t!("settings-about-install-no-package"),
        InstallError::Unreachable => t!("settings-about-install-unreachable"),
        InstallError::Checksum => t!("settings-about-install-checksum"),
        InstallError::Unpack => t!("settings-about-install-unpack"),
        InstallError::NotWritable => t!("settings-about-install-not-writable"),
        InstallError::Replace => t!("settings-about-install-replace"),
        InstallError::Relaunch => t!("settings-about-install-relaunch"),
    }
    .to_string()
}
