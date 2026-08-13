use std::fs;
use std::rc::Rc;

use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui::prelude::{FluentBuilder as _, InteractiveElement as _, StatefulInteractiveElement as _};
use gpui::{
    App, BorrowAppContext as _, Div, Hsla, ParentElement as _, Styled as _, Task, div, px, rgba,
};
use gpui_component::button::Button;
use gpui_component::{
    ActiveTheme as _, Theme as ComponentTheme, ThemeConfig as ComponentThemeConfig,
    ThemeRegistry as ComponentThemeRegistry, ThemeToken as ComponentThemeToken, h_flex, v_flex,
};
use nmt_config::colors::{ColorArray, Colors};
use nmt_config::theme::{AppearanceTheme, Theme, UiTheme};
use nmt_config::{Config, config_dir_path, set_active_colors};
use nmt_i18n::i18n;
use notify::{
    Event as NotifyEvent, RecursiveMode, Result as NotifyResult, Watcher as _, recommended_watcher,
};
use toml::{Table as TomlTable, Value as TomlValue};
use tracing::warn;

use crate::ui::settings::opacity::surface_background_opacity;
use crate::ui::settings::state::AppSettings;
use crate::ui::{UI_BORDER_OPACITY, UI_RADIUS};

/// Apply the UI half of a terminal theme, falling back to the built-in dark
/// palette when the theme does not define `[colors.ui]` or contains invalid UI data.
pub(crate) fn apply_ui_theme(value: Option<&UiTheme>, cx: &mut App) {
    let configured = value.and_then(|value| {
        let mut config = TomlTable::new();

        config.insert("name".to_string(), TomlValue::String(value.name.clone()));
        config.insert(
            "mode".to_string(),
            TomlValue::String(
                match value.mode {
                    AppearanceTheme::Dark => "dark",
                    AppearanceTheme::Light => "light",
                }
                .to_string(),
            ),
        );

        let mut colors = value.colors.clone();

        // Shadow lives at the top level of `ThemeConfig`, while the theme file
        // format keeps it under `[colors.ui]`.
        if let Some(colors) = colors.as_table_mut() {
            if let Some(value) = colors.remove("shadow") {
                config.insert("shadow".to_string(), value);
            }
        }

        config.insert("colors".to_string(), colors);

        TomlValue::Table(config)
            .try_into::<ComponentThemeConfig>()
            .map(Rc::new)
            .map_err(|err| warn!("failed to load UI theme: {err}"))
            .ok()
    });

    let theme = configured.unwrap_or_else(|| {
        ComponentThemeRegistry::global(cx)
            .default_dark_theme()
            .clone()
    });

    let mode = theme.mode;

    ComponentTheme::global_mut(cx).apply_config(&theme);
    ComponentTheme::change(mode, None, cx);

    apply_ui_constants(ComponentTheme::global_mut(cx));
}

fn apply_ui_constants(theme: &mut ComponentTheme) {
    theme.radius = UI_RADIUS;
    theme.radius_lg = UI_RADIUS;
    theme.colors.sidebar_border = theme.colors.sidebar_border.opacity(UI_BORDER_OPACITY);
}

fn select_theme(name: String, cx: &mut App) {
    let theme = if name.is_empty() {
        Ok(Theme::default())
    } else {
        Config::load_named_theme(&name)
    };

    match theme {
        Ok(theme) => {
            set_active_colors(theme.colors.terminal);

            apply_ui_theme(theme.ui_theme().as_ref(), cx);

            cx.update_global(|settings: &mut AppSettings, _| settings.theme = name);

            apply_window_translucency(cx);

            cx.refresh_windows();
        }
        Err(err) => warn!("failed to select theme {name}: {err}"),
    }
}

pub(super) fn load_theme_choices() -> Vec<(String, Theme)> {
    Config::load_themes()
}

fn reload_themes(cx: &mut App) {
    cx.global_mut::<AppSettings>().themes = load_theme_choices();

    let selected = cx.global::<AppSettings>().theme.clone();

    if selected.is_empty() {
        cx.refresh_windows();
    } else {
        select_theme(selected, cx);
    }
}

pub(crate) fn watch_themes(cx: &mut App) -> Option<Task<()>> {
    reload_themes(cx);

    let themes_dir = config_dir_path().join("themes");

    if let Err(err) = fs::create_dir_all(&themes_dir) {
        warn!("failed to create themes directory: {err}");
        return None;
    }

    let (tx, mut rx) = unbounded();

    let mut watcher = match recommended_watcher(move |event: NotifyResult<NotifyEvent>| {
        if event.is_ok() {
            let _ = tx.unbounded_send(());
        }
    }) {
        Ok(watcher) => watcher,
        Err(err) => {
            warn!("failed to watch themes directory: {err}");
            return None;
        }
    };

    if let Err(err) = watcher.watch(&themes_dir, RecursiveMode::NonRecursive) {
        warn!("failed to watch themes directory: {err}");
        return None;
    }

    Some(cx.spawn(async move |cx| {
        let _watcher = watcher;

        while rx.next().await.is_some() {
            let _ = cx.update(reload_themes);
        }
    }))
}

fn preview_color(color: ColorArray) -> Hsla {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;

    rgba(
        channel(color[0]) << 24
            | channel(color[1]) << 16
            | channel(color[2]) << 8
            | channel(color[3]),
    )
    .into()
}

fn theme_preview(colors: Colors) -> Div {
    let swatches = [
        colors.red,
        colors.yellow,
        colors.green,
        colors.cyan,
        colors.blue,
        colors.magenta,
    ];

    v_flex()
        .w_full()
        .h(px(72.0))
        .p_3()
        .gap_2()
        .rounded(UI_RADIUS)
        .bg(preview_color(colors.background.0))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .text_color(preview_color(colors.foreground))
                        .child(i18n("settings-theme-preview-command")),
                )
                .child(
                    div()
                        .text_color(preview_color(colors.blue))
                        .child(i18n("settings-theme-preview-directory")),
                )
                .child(
                    div()
                        .text_color(preview_color(colors.red))
                        .child(i18n("settings-theme-preview-executable")),
                )
                .child(
                    div()
                        .text_color(preview_color(colors.foreground))
                        .child(i18n("settings-theme-preview-file")),
                ),
        )
        .child(h_flex().gap_1().children(swatches.into_iter().map(|color| {
            div()
                .w(px(18.0))
                .h(px(6.0))
                .rounded(UI_RADIUS)
                .bg(preview_color(color))
        })))
}

pub(super) fn theme_list(cx: &mut App) -> Div {
    let selected = cx.global::<AppSettings>().theme.clone();
    let filter = cx.global::<AppSettings>().theme_filter.to_lowercase();

    let themes = cx
        .global::<AppSettings>()
        .themes
        .clone()
        .into_iter()
        .filter(|(name, theme)| {
            let display_name = if name.is_empty() {
                i18n("settings-theme-default")
            } else {
                name
            };
            filter.is_empty()
                || display_name.to_lowercase().contains(&filter)
                || theme.name.to_lowercase().contains(&filter)
        })
        .collect::<Vec<_>>();

    let border = cx.theme().border;
    let selected_border = cx.theme().primary;
    let selected_background = cx.theme().tokens.secondary;
    let hover_background = cx.theme().tokens.secondary_hover;

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .child(i18n("settings-theme-title"))
                .child(
                    Button::new("theme-refresh")
                        .outline()
                        .label(i18n("settings-theme-refresh"))
                        .on_click(|_, _, cx: &mut App| reload_themes(cx)),
                ),
        )
        .when(themes.is_empty(), |this| {
            this.child(
                div()
                    .py_4()
                    .text_color(cx.theme().muted_foreground)
                    .child(i18n("settings-theme-no-matches")),
            )
        })
        .children(
            themes
                .into_iter()
                .enumerate()
                .map(|(index, (name, theme))| {
                    let is_selected = name == selected;

                    let display_name = if theme.name.is_empty() {
                        if name.is_empty() {
                            i18n("settings-theme-default")
                        } else {
                            &name
                        }
                    } else {
                        &theme.name
                    }
                    .to_string();

                    div()
                        .id(("theme-card", index))
                        .w_full()
                        .p_3()
                        .rounded(UI_RADIUS)
                        .border_1()
                        .border_color(if is_selected { selected_border } else { border })
                        .when(is_selected, |this| this.bg(selected_background))
                        .hover(move |this| this.bg(hover_background))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| select_theme(name.clone(), cx))
                        .child(theme_preview(theme.colors.terminal))
                        .child(h_flex().mt_2().justify_between().child(display_name).when(
                            is_selected,
                            |this| {
                                this.child(
                                    div()
                                        .text_color(selected_border)
                                        .child(i18n("settings-theme-selected")),
                                )
                            },
                        ))
                }),
        )
}

pub(super) fn tab_background_opacity(opacity: f32) -> f32 {
    1.0 - (1.0 - opacity) * 0.5
}

/// Retint the component theme for the foreground surface opacity. A configured
/// image shows through by reducing this tint; without an image it remains the
/// effective window opacity. Reset first so repeated calls do not compound alpha.
pub(crate) fn apply_window_translucency(cx: &mut App) {
    let opacity = surface_background_opacity(cx);
    let theme = ComponentTheme::global_mut(cx);

    let palette = if theme.mode.is_dark() {
        theme.dark_theme.clone()
    } else {
        theme.light_theme.clone()
    };

    theme.apply_config(&palette);
    apply_ui_constants(theme);

    if opacity < 1.0 {
        theme.colors.sidebar = theme.colors.sidebar.opacity(opacity);

        // The shell paints this across the whole window as the chrome base
        // layer; it must dim with the rest of the chrome or translucency would
        // be defeated by an opaque backdrop.
        theme.colors.background = theme.colors.background.opacity(opacity);

        for token in [&mut theme.tokens.title_bar, &mut theme.tokens.tab_bar] {
            let color = token.color.opacity(opacity);
            *token = ComponentThemeToken::new(color, color.into());
        }

        // The selected tab needs a stronger fill than the surrounding title-bar
        // chrome, so only half of the configured transparency is applied here.
        let color = theme
            .tokens
            .tab_active
            .color
            .opacity(tab_background_opacity(opacity));
        theme.tokens.tab_active = ComponentThemeToken::new(color, color.into());
    }
}
