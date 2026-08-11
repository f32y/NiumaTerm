use crate::ui::settings::*;

pub(super) fn appearance_page(
    backdrop: WindowBackdrop,
    background_image_enabled: bool,
    tab_auto_size: bool,
) -> SettingPage {
    SettingPage::new("Appearance")
        .default_open(true)
        .group(
            SettingGroup::new()
                .title("Theme")
                .description("Themes are loaded from the themes directory and applied immediately.")
                .item(
                    SettingItem::new(
                        "Make agent pane use terminal's background color",
                        SettingField::switch(
                            |cx| {
                                cx.global::<AppSettings>()
                                    .agent_pane_use_terminal_background
                            },
                            |value, cx| {
                                cx.global_mut::<AppSettings>()
                                    .agent_pane_use_terminal_background = value;
                            },
                        ),
                    )
                    .description("Use the terminal theme's background color for Agent Pane."),
                )
                .item(
                    SettingItem::new(
                        "Search",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().theme_filter.clone().into(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().theme_filter = value.to_string();
                            },
                        ),
                    )
                    .description("Filter themes by file name or UI theme name."),
                )
                .item(
                    SettingItem::render(|_, _, cx| theme_list(cx))
                        .keywords(["theme", "colors", "palette"]),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Window")
                .item(
                    SettingItem::new(
                        "Window Backdrop",
                        SettingField::dropdown(
                            vec![
                                ("mica".into(), "Mica".into()),
                                ("acrylic".into(), "Acrylic".into()),
                                ("off".into(), "Off".into()),
                            ],
                            |cx| cx.global::<AppSettings>().window_backdrop.as_str().into(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().window_backdrop =
                                    WindowBackdrop::from_value(&value);
                            },
                        )
                        .default_value(SharedString::from("acrylic")),
                    )
                    .description(
                        "Window backdrop material. Acrylic blurs the content behind the window; \
                         Mica is a static Windows 11 tint; Off keeps the window opaque.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Transparent Main View",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().transparent_main_view,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().transparent_main_view = value;
                            },
                        ),
                    )
                    .description("Use a translucent background for Terminal View and Agent Pane."),
                )
                .item(
                    SettingItem::new(
                        "Smooth Scrolling",
                        SettingField::dropdown(
                            vec![
                                ("all".into(), "All".into()),
                                ("only-terminal".into(), "Only Terminal".into()),
                                ("only-agent".into(), "Only Agent".into()),
                                ("off".into(), "Off".into()),
                            ],
                            |cx| cx.global::<AppSettings>().smooth_scrolling.as_str().into(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().smooth_scrolling =
                                    SmoothScrollingMode::from_value(&value);
                            },
                        )
                        .default_value(SharedString::from("all")),
                    )
                    .description("Choose where traditional mouse-wheel scrolling is animated."),
                )
                .item(
                    SettingItem::new("Background Opacity", background_opacity_field())
                        .description(
                            "Whole-window opacity while a translucent backdrop is selected.",
                        )
                        .disabled(backdrop == WindowBackdrop::Off),
                )
                .item(
                    SettingItem::new("Background Image", background_image_field())
                        .description("Local image stretched to cover the whole window."),
                )
                .item(
                    SettingItem::new("Background Image Opacity", background_image_opacity_field())
                        .description("How strongly the image shows through window surfaces.")
                        .disabled(!background_image_enabled),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Font")
                .item(
                    SettingItem::new(
                        "UI Font",
                        ui::font_picker::font_family_field(ui::font_picker::FontTarget::Ui),
                    )
                    .description("Font for the app chrome (titlebar, sidebar, tabs, dialogs)."),
                )
                .item(
                    SettingItem::new(
                        "Terminal Font",
                        ui::font_picker::font_family_field(ui::font_picker::FontTarget::Terminal),
                    )
                    .description("Font used by the terminal view."),
                )
                .item(
                    SettingItem::new(
                        "Terminal Font Size",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 6.0,
                                max: 72.0,
                                step: 0.1,
                            },
                            |cx| cx.global::<AppSettings>().terminal_font_size,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().terminal_font_size = value;
                            },
                        ),
                    )
                    .description("Font size in pixels."),
                )
                .item(
                    SettingItem::new(
                        "Terminal Line Height",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 0.8,
                                max: 3.0,
                                step: 0.1,
                            },
                            |cx| cx.global::<AppSettings>().terminal_line_height,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().terminal_line_height = value;
                            },
                        ),
                    )
                    .description("Line height as a multiplier on font size."),
                )
                .item(
                    SettingItem::new(
                        "Agent Font",
                        ui::font_picker::font_family_field(ui::font_picker::FontTarget::Agent),
                    )
                    .description("Font used by agent (chat) tabs."),
                )
                .item(
                    SettingItem::new(
                        "Agent Font Size",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 6.0,
                                max: 72.0,
                                step: 0.1,
                            },
                            |cx| cx.global::<AppSettings>().agent_font_size,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().agent_font_size = value;
                            },
                        ),
                    )
                    .description("Font size in pixels."),
                )
                .item(
                    SettingItem::new(
                        "Show monospace fonts only",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().monospace_only,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().monospace_only = value;
                            },
                        ),
                    )
                    .description("Filter the font list to fixed-width fonts."),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Tab Bar")
                .item(
                    SettingItem::new(
                        "Auto Size",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().tab_auto_size,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().tab_auto_size = value;
                            },
                        ),
                    )
                    .description(
                        "Narrow tabs as the strip fills, down to the leading icon. \
                         Turn off to hold a fixed width.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Tab Width",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: DEFAULT_TAB_WIDTH,
                                max: MAX_TAB_WIDTH,
                                step: 1.0,
                            },
                            |cx| cx.global::<AppSettings>().tab_width,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().tab_width = clamp_tab_width(value);
                            },
                        ),
                    )
                    // Auto Size derives the width from the strip, so the entry
                    // would report a value the tabs no longer use.
                    .disabled(tab_auto_size)
                    .description("Fixed tab width in pixels; long titles are clipped."),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Title Bar")
                .item(
                    SettingItem::new(
                        "Show daily token usage",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().show_daily_token_usage,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().show_daily_token_usage = value;
                            },
                        ),
                    )
                    .description(
                        "Show today's ccusage token totals in the titlebar, \
                             refreshed every 60 seconds (click to refresh now).",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Show Git Status on Title Bar",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().show_git_status_on_title_bar,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().show_git_status_on_title_bar = value;
                            },
                        ),
                    )
                    .description(
                        "Show the active repository's +added -removed line \
                             counts in the titlebar.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Git Status Refresh Interval",
                        SettingField::dropdown(
                            vec![
                                ("10".into(), "10s".into()),
                                ("15".into(), "15s".into()),
                                ("30".into(), "30s".into()),
                                ("60".into(), "60s".into()),
                            ],
                            |cx| {
                                cx.global::<AppSettings>()
                                    .git_status_refresh_interval
                                    .to_string()
                                    .into()
                            },
                            |value, cx| {
                                cx.global_mut::<AppSettings>().git_status_refresh_interval =
                                    clamp_git_interval(value.parse().unwrap_or(30));
                            },
                        )
                        .default_value(SharedString::from("30")),
                    )
                    .description("How often the git status is re-read."),
                ),
        )
}
