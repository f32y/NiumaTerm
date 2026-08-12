use crate::ui::settings::*;

pub(super) fn terminal_page() -> SettingPage {
    SettingPage::new("Terminal").default_open(true).group(
        SettingGroup::new()
            .title("Input")
            .item(
                SettingItem::new(
                    "Input Style",
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
                )
                .description("How the prompt input is presented."),
            )
            .item(
                SettingItem::new(
                    "Cursor Shape",
                    SettingField::dropdown(
                        vec![
                            ("block".into(), "Block".into()),
                            ("line".into(), "Line".into()),
                            ("underline".into(), "Underline".into()),
                        ],
                        |cx| cx.global::<AppSettings>().cursor_shape.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().cursor_shape =
                                cursor_shape_from_value(&value);
                        },
                    )
                    .default_value(SharedString::from("block")),
                )
                .description("Default cursor shape used by newly opened terminals."),
            )
            .item(
                SettingItem::new(
                    "Command Blocks",
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().command_blocks,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().command_blocks = value;
                        },
                    ),
                )
                .description(
                    "Group each command's output into a block with a separator, \
                                 exit status, and duration. Off: outputs run together like a \
                                 classic terminal.",
                ),
            )
            .item(
                SettingItem::new(
                    "Scroll to bottom when typing",
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().scroll_to_bottom_when_typing,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().scroll_to_bottom_when_typing = value;
                        },
                    ),
                )
                .description("Show the latest terminal output when typing into a scrolled view."),
            ),
    )
}
