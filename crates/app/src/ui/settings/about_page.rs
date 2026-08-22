use nmt_i18n::i18n;

use crate::ui::settings::*;
use crate::update::{self, CheckError, Status};

pub(super) fn about_page() -> SettingPage {
    SettingPage::new(i18n("settings-about-title"))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(i18n("settings-about-product"))
                .item(SettingItem::new(
                    i18n("settings-about-version"),
                    SettingField::render(|_, _, _| Label::new(APP_VERSION).text_sm()),
                ))
                .item(SettingItem::new(
                    i18n("settings-about-releases"),
                    SettingField::render(|_, _, _| {
                        Button::new("go-to-release-page")
                            .outline()
                            .label(i18n("settings-about-release-page"))
                            .on_click(|_, _, cx: &mut App| cx.open_url(RELEASE_PAGE_URL))
                    }),
                )),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-about-updates"))
                .item(
                    SettingItem::new(
                        i18n("settings-about-check-updates"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().check_updates,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().check_updates = value;
                            },
                        ),
                    )
                    .description(i18n("settings-about-check-updates-description")),
                )
                .item(SettingItem::new(
                    i18n("settings-about-channel"),
                    SettingField::dropdown(
                        vec![
                            (
                                "stable".into(),
                                i18n("settings-about-channel-stable").into(),
                            ),
                            (
                                "nightly".into(),
                                i18n("settings-about-channel-nightly").into(),
                            ),
                        ],
                        |cx| cx.global::<AppSettings>().update_channel.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().update_channel =
                                UpdateChannel::from_value(&value);
                        },
                    )
                    .default_value(SharedString::from("stable")),
                ))
                .item(update_check_item()),
        )
}

fn update_check_item() -> SettingItem {
    SettingItem::render(move |options, _window, cx| {
        let status = update::status(cx);
        let checking = status == Status::Checking;

        let check = Button::new("app-update-check")
            .outline()
            .label(if checking {
                i18n("settings-about-checking")
            } else {
                i18n("settings-about-check-button")
            })
            .disabled(options.disabled || checking)
            .on_click(|_, _, cx: &mut App| update::check_now(cx));

        // The channel resolved a specific release, so the link goes to that
        // one rather than to the list the user would have to find it in.
        let open = match &status {
            Status::Available(release) => {
                let page_url = release.page_url.clone();
                Some(
                    Button::new("app-update-open")
                        .primary()
                        .label(i18n("settings-about-open-release"))
                        .disabled(options.disabled)
                        .on_click(move |_, _, cx: &mut App| cx.open_url(&page_url)),
                )
            }
            _ => None,
        };

        card_row(
            i18n("settings-about-check-for-updates"),
            status_text(&status),
            h_flex().gap_2().children(open).child(check),
            cx,
        )
        .into_any_element()
    })
}

fn status_text(status: &Status) -> String {
    match status {
        Status::Unknown => i18n("settings-about-not-checked").to_string(),
        Status::Checking => i18n("settings-about-checking").to_string(),
        Status::NothingPublished => i18n("settings-about-nothing-published").to_string(),
        Status::UpToDate => i18n("settings-about-up-to-date").replace("{version}", APP_VERSION),
        Status::Available(release) => {
            i18n("settings-about-update-available").replace("{version}", &release.label)
        }
        Status::Failed(CheckError::Unreachable) => {
            i18n("settings-about-check-unreachable").to_string()
        }
        Status::Failed(CheckError::Unreadable) => {
            i18n("settings-about-check-unreadable").to_string()
        }
    }
}
