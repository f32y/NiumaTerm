use nmt_i18n::i18n;

use crate::ui::settings::*;

pub(super) fn terminal_page() -> SettingPage {
    SettingPage::new(i18n("settings-terminal-title"))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(i18n("settings-terminal-input"))
                .item(SettingItem::new(
                    i18n("settings-terminal-input-style"),
                    SettingField::dropdown(
                        vec![
                            (
                                InputStyle::Waterfall.as_str().into(),
                                input_style_label(InputStyle::Waterfall).into(),
                            ),
                            (
                                InputStyle::FixedBottom.as_str().into(),
                                input_style_label(InputStyle::FixedBottom).into(),
                            ),
                        ],
                        |cx| cx.global::<AppSettings>().input_style.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().input_style =
                                input_style_from_value(&value);
                        },
                    )
                    .default_value(SharedString::from(InputStyle::Waterfall.as_str())),
                ))
                .item(SettingItem::new(
                    i18n("settings-terminal-cursor-shape"),
                    SettingField::dropdown(
                        vec![
                            (
                                "block".into(),
                                i18n("settings-terminal-cursor-block").into(),
                            ),
                            ("line".into(), i18n("settings-terminal-cursor-line").into()),
                            (
                                "underline".into(),
                                i18n("settings-terminal-cursor-underline").into(),
                            ),
                        ],
                        |cx| cx.global::<AppSettings>().cursor_shape.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().cursor_shape =
                                cursor_shape_from_value(&value);
                        },
                    )
                    .default_value(SharedString::from("block")),
                ))
                .item(SettingItem::new(
                    i18n("settings-terminal-command-blocks"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().command_blocks,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().command_blocks = value;
                        },
                    ),
                ))
                .item(SettingItem::new(
                    i18n("settings-terminal-scroll-on-typing"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().scroll_to_bottom_when_typing,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().scroll_to_bottom_when_typing = value;
                        },
                    ),
                )),
        )
}
