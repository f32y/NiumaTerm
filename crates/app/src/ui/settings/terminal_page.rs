use gpui_component::setting::{SettingGroup, SettingItem, SettingPage};
use rust_i18n::t;

use crate::ui::settings::fields::{settings_choice, settings_switch};
use crate::ui::settings::state::{InputStyle, input_style_label};

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
                    settings_choice(
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
                        |config| config.appearance.input_style.into(),
                        |settings, value| {
                            settings.edit_appearance(|section| section.input_style = value.into());
                        },
                    )
                    .default_value(waterfall_key),
                ))
                .item(SettingItem::new(
                    t!("settings-terminal-cursor-shape"),
                    settings_choice(
                        vec![
                            ("block".into(), t!("settings-terminal-cursor-block").into()),
                            ("line".into(), t!("settings-terminal-cursor-line").into()),
                            (
                                "underline".into(),
                                t!("settings-terminal-cursor-underline").into(),
                            ),
                        ],
                        |config| config.cursor.shape.into(),
                        |settings, value| {
                            settings.set_cursor_shape(value.into());
                        },
                    )
                    .default_value("block"),
                ))
                .item(SettingItem::new(
                    t!("settings-terminal-command-blocks"),
                    settings_switch(
                        |config| config.appearance.command_blocks,
                        |settings, value| {
                            settings.edit_appearance(|section| section.command_blocks = value);
                        },
                    ),
                ))
                .item(SettingItem::new(
                    t!("settings-terminal-scroll-on-typing"),
                    settings_switch(
                        |config| config.appearance.scroll_to_bottom_when_typing,
                        |settings, value| {
                            settings.edit_appearance(|section| {
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
                        settings_switch(
                            |config| config.terminal.improve_powershell_compatibility,
                            |settings, value| {
                                settings.edit_terminal(|section| {
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
