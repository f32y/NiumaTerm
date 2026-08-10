use crate::ui::settings::*;

pub(super) fn about_page() -> SettingPage {
    SettingPage::new("About").default_open(true).group(
        SettingGroup::new()
            .title("NiumaTerm")
            .item(SettingItem::new(
                "Version",
                SettingField::render(|_, _, _| Label::new(APP_VERSION).text_sm()),
            ))
            .item(SettingItem::new(
                "Releases",
                SettingField::render(|_, _, _| {
                    Button::new("go-to-release-page")
                        .outline()
                        .label("Go to Release Page")
                        .on_click(|_, _, cx: &mut App| cx.open_url(RELEASE_PAGE_URL))
                }),
            )),
    )
}
