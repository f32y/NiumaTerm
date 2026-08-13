use nmt_i18n::i18n;

use crate::ui::settings::*;

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
}
