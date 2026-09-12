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
                            let key: &str =
                                cx.global::<AppSettings>().appearance.input_style.into();

                            key.into()
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>().appearance.input_style =
                                value.as_str().into();
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
                            let key: &str = cx.global::<AppSettings>().cursor_shape.into();

                            key.into()
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>().cursor_shape = value.as_str().into();
                        },
                    )
                    .default_value("block"),
                ))
                .item(SettingItem::new(
                    t!("settings-terminal-command-blocks"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().appearance.command_blocks,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().appearance.command_blocks = value;
                        },
                    ),
                ))
                .item(SettingItem::new(
                    t!("settings-terminal-scroll-on-typing"),
                    SettingField::switch(
                        |cx| {
                            cx.global::<AppSettings>()
                                .appearance
                                .scroll_to_bottom_when_typing
                        },
                        |value, cx| {
                            cx.global_mut::<AppSettings>()
                                .appearance
                                .scroll_to_bottom_when_typing = value;
                        },
                    ),
                )),
        )
}
