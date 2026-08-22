use nmt_i18n::i18n;

use crate::ui::settings::*;

pub(super) fn appearance_page(
    backdrop: WindowBackdrop,
    background_image_enabled: bool,
    tab_auto_size: bool,
    show_git_status: bool,
) -> SettingPage {
    SettingPage::new(i18n("settings-appearance-title"))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(i18n("settings-appearance-theme"))
                .item(SettingItem::new(
                    i18n("settings-appearance-agent-pane-terminal-background"),
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
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-theme-search"),
                    SettingField::input(
                        |cx| cx.global::<AppSettings>().theme_filter.clone().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().theme_filter = value.to_string();
                        },
                    ),
                ))
                .item(SettingItem::render(|_, _, cx| theme_list(cx)).keywords([
                    i18n("settings-appearance-keyword-theme"),
                    i18n("settings-appearance-keyword-colors"),
                    i18n("settings-appearance-keyword-palette"),
                ])),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-appearance-window"))
                .item(SettingItem::new(
                    i18n("settings-appearance-window-backdrop"),
                    SettingField::dropdown(
                        vec![
                            (
                                "mica-alt".into(),
                                i18n("settings-appearance-window-backdrop-mica-alt").into(),
                            ),
                            (
                                "mica".into(),
                                i18n("settings-appearance-window-backdrop-mica").into(),
                            ),
                            (
                                "acrylic".into(),
                                i18n("settings-appearance-window-backdrop-acrylic").into(),
                            ),
                            ("off".into(), i18n("settings-common-off").into()),
                        ],
                        |cx| cx.global::<AppSettings>().window_backdrop.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().window_backdrop =
                                WindowBackdrop::from_value(&value);
                        },
                    )
                    .default_value(SharedString::from("acrylic")),
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-effect-on-content-area"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().transparent_main_view,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().transparent_main_view = value;
                        },
                    ),
                ))
                .item(
                    SettingItem::new(
                        i18n("settings-appearance-background-opacity"),
                        background_opacity_field(),
                    )
                    // The Mica materials replace the background with what DWM
                    // draws, so a custom opacity has nothing left to act on.
                    .disabled(matches!(
                        backdrop,
                        WindowBackdrop::Off | WindowBackdrop::MicaAlt | WindowBackdrop::Mica
                    )),
                )
                .item(SettingItem::new(
                    i18n("settings-appearance-background-image"),
                    background_image_field(),
                ))
                .item(
                    SettingItem::new(
                        i18n("settings-appearance-background-image-opacity"),
                        background_image_opacity_field(),
                    )
                    .disabled(!background_image_enabled),
                )
                .item(SettingItem::new(
                    i18n("settings-appearance-smooth-scrolling"),
                    SettingField::dropdown(
                        vec![
                            (
                                "all".into(),
                                i18n("settings-appearance-scrolling-all").into(),
                            ),
                            (
                                "only-terminal".into(),
                                i18n("settings-appearance-scrolling-only-terminal").into(),
                            ),
                            (
                                "only-agent".into(),
                                i18n("settings-appearance-scrolling-only-agent").into(),
                            ),
                            ("off".into(), i18n("settings-common-off").into()),
                        ],
                        |cx| cx.global::<AppSettings>().smooth_scrolling.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().smooth_scrolling =
                                SmoothScrollingMode::from_value(&value);
                        },
                    )
                    .default_value(SharedString::from("all")),
                )),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-appearance-font"))
                .item(SettingItem::new(
                    i18n("settings-appearance-ui-font"),
                    ui::font_picker::font_family_field(ui::font_picker::FontTarget::Ui),
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-terminal-font"),
                    ui::font_picker::font_family_field(ui::font_picker::FontTarget::Terminal),
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-terminal-font-size"),
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
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-terminal-line-height"),
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
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-agent-font"),
                    ui::font_picker::font_family_field(ui::font_picker::FontTarget::Agent),
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-agent-font-size"),
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
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-monospace-only"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().monospace_only,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().monospace_only = value;
                        },
                    ),
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-agent-transcript-font"),
                    ui::font_picker::font_family_field(
                        ui::font_picker::FontTarget::AgentTranscript,
                    ),
                ))
                .item(SettingItem::new(
                    i18n("settings-appearance-agent-transcript-font-size"),
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 6.0,
                            max: 72.0,
                            step: 0.1,
                        },
                        |cx| cx.global::<AppSettings>().agent_transcript_font_size,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().agent_transcript_font_size = value;
                        },
                    ),
                )),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-appearance-tab-bar"))
                .item(
                    SettingItem::new(
                        i18n("settings-appearance-tab-bar-style"),
                        SettingField::dropdown(
                            vec![
                                (
                                    "horizontal".into(),
                                    i18n("settings-appearance-tab-bar-style-horizontal").into(),
                                ),
                                (
                                    "vertical".into(),
                                    i18n("settings-appearance-tab-bar-style-vertical").into(),
                                ),
                            ],
                            |cx| cx.global::<AppSettings>().tab_bar_style.as_str().into(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().tab_bar_style =
                                    TabBarStyle::from_value(&value);
                            },
                        )
                        .default_value(SharedString::from("horizontal")),
                    )
                    .description(i18n("settings-appearance-tab-bar-style-description")),
                )
                .item(SettingItem::new(
                    i18n("settings-appearance-tab-auto-size"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().tab_auto_size,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().tab_auto_size = value;
                        },
                    ),
                ))
                .item(
                    SettingItem::new(
                        i18n("settings-appearance-tab-width"),
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
                    .disabled(tab_auto_size),
                ),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-appearance-title-bar"))
                .item(
                    SettingItem::new(
                        i18n("settings-appearance-daily-token-usage"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().show_daily_token_usage,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().show_daily_token_usage = value;
                            },
                        ),
                    )
                    .description(i18n("settings-appearance-daily-token-usage-description")),
                )
                .item(SettingItem::new(
                    i18n("settings-appearance-git-status"),
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().show_git_status_on_title_bar,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().show_git_status_on_title_bar = value;
                        },
                    ),
                ))
                .item(
                    SettingItem::new(
                        i18n("settings-appearance-git-interval"),
                        SettingField::dropdown(
                            vec![
                                (
                                    "10".into(),
                                    i18n("settings-appearance-git-interval-10").into(),
                                ),
                                (
                                    "15".into(),
                                    i18n("settings-appearance-git-interval-15").into(),
                                ),
                                (
                                    "30".into(),
                                    i18n("settings-appearance-git-interval-30").into(),
                                ),
                                (
                                    "60".into(),
                                    i18n("settings-appearance-git-interval-60").into(),
                                ),
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
                    // The titlebar switch is the feature's master toggle;
                    // while it is off the interval has no display to pace, so
                    // the entry is locked instead of reporting a dead value.
                    .disabled(!show_git_status),
                ),
        )
        .group(
            SettingGroup::new()
                .title(i18n("settings-appearance-language"))
                .item(SettingItem::new(
                    i18n("settings-appearance-language"),
                    SettingField::dropdown(
                        vec![
                            // Language names are proper nouns shown in their
                            // own language, so they stay out of the catalogs.
                            ("en".into(), "English".into()),
                            ("zh-CN".into(), "简体中文".into()),
                        ],
                        |cx| cx.global::<AppSettings>().language.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().language = Language::from_value(&value);
                        },
                    )
                    .default_value(SharedString::from("en")),
                )),
        )
}
