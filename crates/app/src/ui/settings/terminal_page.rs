use rust_i18n::t;

use crate::ui::settings::*;

pub(super) fn terminal_page() -> SettingPage {
    let waterfall_key: &str = InputStyle::Waterfall.into();
    let fixed_bottom_key: &str = InputStyle::FixedBottom.into();

    SettingPage::new(t!("settings-terminal-title"))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(t!("settings-terminal-input"))
                .item(SettingItem::new(
                    t!("settings-terminal-input-style"),
                    SettingField::dropdown(
                        vec![
                            (
                                waterfall_key.into(),
                                input_style_label(InputStyle::Waterfall).into(),
                            ),
                            (
                                fixed_bottom_key.into(),
                                input_style_label(InputStyle::FixedBottom).into(),
                            ),
                        ],
                        |cx| {
                            let key: &str = cx
                                .global::<AppSettings>()
                                .config()
                                .appearance
                                .input_style
                                .into();

                            key.into()
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>().edit_appearance(|section| {
                                section.input_style = value.as_str().into()
                            });
                        },
                    )
                    .default_value(waterfall_key),
                ))
                .item(SettingItem::new(
                    t!("settings-terminal-cursor-shape"),
                    SettingField::dropdown(
                        vec![
                            ("block".into(), t!("settings-terminal-cursor-block").into()),
                            ("line".into(), t!("settings-terminal-cursor-line").into()),
                            (
                                "underline".into(),
                                t!("settings-terminal-cursor-underline").into(),
                            ),
                        ],
                        |cx| {
                            let key: &str = cx.global::<AppSettings>().config().cursor.shape.into();

                            key.into()
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>()
                                .set_cursor_shape(value.as_str().into());
                        },
                    )
                    .default_value("block"),
                ))
                .item(SettingItem::new(
                    t!("settings-terminal-command-blocks"),
                    SettingField::switch(
                        |cx| {
                            cx.global::<AppSettings>()
                                .config()
                                .appearance
                                .command_blocks
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>()
                                .edit_appearance(|section| section.command_blocks = value);
                        },
                    ),
                ))
                .item(SettingItem::new(
                    t!("settings-terminal-scroll-on-typing"),
                    SettingField::switch(
                        |cx| {
                            cx.global::<AppSettings>()
                                .config()
                                .appearance
                                .scroll_to_bottom_when_typing
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>().edit_appearance(|section| {
                                section.scroll_to_bottom_when_typing = value
                            });
                        },
                    ),
                )),
        )
        .group(
            SettingGroup::new()
                .title(t!("settings-terminal-advanced"))
                .item(
                    SettingItem::new(
                        t!("settings-terminal-powershell-compatibility"),
                        SettingField::switch(
                            |cx| {
                                cx.global::<AppSettings>()
                                    .config()
                                    .terminal
                                    .improve_powershell_compatibility
                            },
                            |value, cx| {
                                cx.global_mut::<AppSettings>().edit_terminal(|section| {
                                    section.improve_powershell_compatibility = value;
                                });
                            },
                        ),
                    )
                    .description(
                        t!("settings-terminal-powershell-compatibility-description").into_owned(),
                    ),
                ),
        )
}
