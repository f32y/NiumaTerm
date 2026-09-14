use std::fs;
use std::rc::Rc;
use std::time::Duration;

use app::design::{CARD_RADIUS, CONTROL_RADIUS};

use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui::{App, BorrowAppContext as _, Entity, Task};
use gpui_component::{
    Theme as ComponentTheme, ThemeConfig as ComponentThemeConfig,
    ThemeRegistry as ComponentThemeRegistry, ThemeToken as ComponentThemeToken,
};
use nmt_config::theme::{AppearanceTheme, Theme, UiTheme};
use nmt_config::{Config, config_dir_path, set_active_colors};
use notify::{
    Event as NotifyEvent, RecursiveMode, Result as NotifyResult, Watcher as _, recommended_watcher,
};
use toml::{Table as TomlTable, Value as TomlValue};
use tracing::warn;

use crate::ui::UI_BORDER_OPACITY;
use crate::ui::fluent::BUTTON_PADDING_X;
use crate::ui::settings::opacity::{main_view_background_opacity, surface_background_opacity};
use crate::ui::settings::state::{AppSettings, SettingsEditing};

/// Apply the UI half of a terminal theme, falling back to the built-in dark
/// palette when the theme does not define `[colors.ui]` or contains invalid UI data.
pub(crate) fn apply_ui_theme(value: Option<&UiTheme>, cx: &mut App) {
    let configured = value.and_then(ui_theme_config);

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

/// Translate the `[colors.ui]` section of a theme file into the component
/// library's theme configuration, dropping the theme when a key does not parse.
pub(super) fn ui_theme_config(value: &UiTheme) -> Option<Rc<ComponentThemeConfig>> {
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

    // Shadow, the corner radii, and the syntax palette live at the top level
    // of `ThemeConfig`, while the theme file format keeps them under
    // `[colors.ui]`. The color table's deserializer drops keys it does not
    // know, so a palette left in place would vanish silently.
    if let Some(colors) = colors.as_table_mut() {
        for key in ["shadow", "radius", "radius.lg", "highlight"] {
            if let Some(value) = colors.remove(key) {
                config.insert(key.to_string(), value);
            }
        }
    }

    config.insert("colors".to_string(), colors);

    TomlValue::Table(config)
        .try_into::<ComponentThemeConfig>()
        .map(Rc::new)
        .map_err(|err| warn!("failed to load UI theme: {err}"))
        .ok()
}

fn apply_ui_constants(theme: &mut ComponentTheme) {
    // Geometry belongs to the application: switching a palette must not
    // change the shape of controls, tabs, or conversation cards.
    theme.radius = CONTROL_RADIUS;
    theme.radius_lg = CARD_RADIUS;

    // No theme-file key backs this one, so it is not a fallback: button
    // padding is a property of the application's design language rather than
    // of the palette a theme chooses.
    theme.button_padding_x = BUTTON_PADDING_X;
    theme.colors.sidebar_border = theme.colors.sidebar_border.opacity(UI_BORDER_OPACITY);
}

pub(super) fn select_theme(name: String, cx: &mut App) -> bool {
    let theme = if name.is_empty() {
        Ok(Theme::default())
    } else {
        Config::load_named_theme(&name)
    };

    match theme {
        Ok(theme) => {
            if let Some(ui) = theme.ui_theme()
                && ui_theme_config(&ui).is_none()
            {
                return false;
            }

            set_active_colors(theme.colors.terminal);

            apply_ui_theme(theme.ui_theme().as_ref(), cx);

            cx.update_global(|settings: &mut AppSettings, _| settings.set_theme(name));

            apply_window_translucency(cx);

            cx.refresh_windows();

            true
        }
        Err(err) => {
            warn!("failed to select theme {name}: {err}");

            false
        }
    }
}

fn reload_themes(editing: &Entity<SettingsEditing>, cx: &mut App) {
    let selected = cx.global::<AppSettings>().config().theme.clone();
    let applied = select_theme(selected.clone(), cx);

    let mut themes = Config::load_themes();

    editing.update(cx, |editing, cx| {
        // A partial file save must not remove the active card or replace the
        // currently displayed palette with a fallback.
        if !applied {
            themes.retain(|(id, _)| id != &selected);

            if let Some(previous) = editing.themes.iter().find(|(id, _)| id == &selected) {
                themes.push(previous.clone());
            }
        }

        editing.themes = themes;
        editing.theme_load_failed = !applied;

        cx.notify();
    });
}

pub(crate) fn watch_themes(editing: &Entity<SettingsEditing>, cx: &mut App) -> Option<Task<()>> {
    reload_themes(editing, cx);

    let themes_dir = config_dir_path().join("themes");

    if let Err(err) = fs::create_dir_all(&themes_dir) {
        warn!("failed to create themes directory: {err}");

        return None;
    }

    let (tx, mut rx) = unbounded();

    let mut watcher = match recommended_watcher(move |event: NotifyResult<NotifyEvent>| {
        if let Ok(event) = event
            && (event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove())
        {
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

    let editing = editing.downgrade();

    Some(cx.spawn(async move |cx| {
        let _watcher = watcher;

        while rx.next().await.is_some() {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;

            while rx.try_recv().is_ok() {}

            let Some(editing) = editing.upgrade() else {
                break;
            };

            cx.update(|cx| reload_themes(&editing, cx));
        }
    }))
}

/// Retint the component theme for the foreground surface opacity. A configured
/// image shows through by reducing this tint; without an image it remains the
/// effective window opacity. Reset first so repeated calls do not compound alpha.
pub(crate) fn apply_window_translucency(cx: &mut App) {
    let opacity = surface_background_opacity(cx);

    // The sidebar color surfaces the agent pane and the right-hand panel, both
    // of which live inside a tab, so it follows the content-area switch instead
    // of the chrome opacity. Otherwise turning the switch off would still leave
    // those panels see-through.
    let content_opacity = main_view_background_opacity(cx);
    let theme = ComponentTheme::global_mut(cx);

    let palette = if theme.mode.is_dark() {
        theme.dark_theme.clone()
    } else {
        theme.light_theme.clone()
    };

    theme.apply_config(&palette);

    apply_ui_constants(theme);

    if content_opacity < 1.0 {
        theme.colors.sidebar = theme.colors.sidebar.opacity(content_opacity);
    }

    if opacity < 1.0 {
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
            .opacity(1.0 - (1.0 - opacity) * 0.5);

        theme.tokens.tab_active = ComponentThemeToken::new(color, color.into());
    }
}
