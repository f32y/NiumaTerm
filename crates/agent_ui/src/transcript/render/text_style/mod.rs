//! How transcript prose and code are styled, from the configured font down to
//! the markdown view every text row is built on.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, ElementId, Font, Hsla, SharedString, StyleRefinement, px};
use gpui_component::highlighter::HighlightTheme;
use gpui_component::text::TextViewStyle;
use gpui_component::{ActiveTheme as _, text};

use crate::settings::AgentSettings;

/// Assistant reply: bare markdown — no bubble, no border; alignment and
/// surface carry the distinction.
pub(crate) fn transcript_code_block_style(font: Font, font_size: f32) -> StyleRefinement {
    StyleRefinement::default()
        .font(font)
        .text_size(px(font_size))
}

fn configured_transcript_code_block_style(cx: &App) -> StyleRefinement {
    let settings = cx.global::<AgentSettings>();

    transcript_code_block_style(settings.transcript_font(), settings.transcript_font_size)
}

/// The rich-text layer fills every markdown table with the popover surface
/// color. On palettes whose popover is lighter than the pane fill (the gray
/// light themes) that fill reads as a white card floating over the
/// transcript. The theme's table background is the color a palette assigns to
/// table surfaces; every shipped theme keeps it transparent so the table sits
/// directly on the transcript fill and follows its translucency.
fn transcript_table_style(cx: &App) -> StyleRefinement {
    StyleRefinement::default().bg(cx.theme().table)
}

/// The syntax palette for code painted in the pane.
///
/// With the pane painted over the terminal background, the surface under a
/// code block is the terminal's, and a theme file may put that on the other
/// side of mid-gray from its UI colors. The UI theme's palette was chosen for
/// the UI surface, so on a surface of the opposite brightness the component
/// library's built-in palette for that brightness stands in; the theme's own
/// palette is kept whenever the two agree.
pub(crate) fn transcript_highlight_theme(cx: &App) -> Arc<HighlightTheme> {
    let themed = cx.theme().highlight_theme.clone();

    if !cx
        .global::<AgentSettings>()
        .pane_background_follows_terminal
    {
        return themed;
    }

    let surface = cx.global::<AgentSettings>().terminal_background;

    highlight_theme_for_surface(themed, is_dark_surface(surface))
}

pub(crate) fn highlight_theme_for_surface(
    themed: Arc<HighlightTheme>,
    surface_is_dark: bool,
) -> Arc<HighlightTheme> {
    if themed.appearance.is_dark() == surface_is_dark {
        themed
    } else if surface_is_dark {
        HighlightTheme::default_dark()
    } else {
        HighlightTheme::default_light()
    }
}

/// Mid-gray in HSL lightness splits dark surfaces from light ones. A theme
/// file's mode is checked against its palette with the same measure, so a
/// palette and the surface it lands on agree on which side they are on.
pub(crate) fn is_dark_surface(color: Hsla) -> bool {
    color.l < 0.5
}

pub(crate) fn transcript_text_style(cx: &App) -> TextViewStyle {
    let mut style = TextViewStyle::default()
        .code_block(configured_transcript_code_block_style(cx))
        .table(transcript_table_style(cx));

    style.highlight_theme = transcript_highlight_theme(cx);

    style
}

pub(crate) fn markdown_view(
    id: impl Into<ElementId>,
    markdown: impl Into<SharedString>,
    cwd: Option<String>,
) -> text::TextView {
    text::TextView::markdown(id, markdown).on_link_click(move |target, _, _, cx| {
        open_link(target, cwd.as_deref().map(Path::new), cx);
    })
}

fn open_link(target: &str, cwd: Option<&Path>, cx: &mut App) {
    if let Some(path) = resolve_local_path(target, cwd).filter(|path| path.exists()) {
        cx.open_with_system(&path);
    } else {
        cx.open_url(target);
    }
}

fn resolve_local_path(target: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let target = strip_source_position(target.trim());

    if target.is_empty() || has_uri_scheme(target) {
        return None;
    }

    let target = strip_extra_drive_slash(target);
    let path: PathBuf = target.into();

    if path.is_absolute() || has_windows_root(target) {
        Some(path)
    } else {
        cwd.map(|cwd| cwd.join(path))
    }
}

fn strip_source_position(mut target: &str) -> &str {
    for _ in 0..2 {
        let Some((path, position)) = target.rsplit_once(':') else {
            break;
        };

        if position.is_empty() || !position.bytes().all(|byte| byte.is_ascii_digit()) {
            break;
        }

        target = path;
    }

    target
}

fn has_uri_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };

    if scheme.len() == 1 {
        return false;
    }

    let mut chars = scheme.chars();

    chars.next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn has_windows_root(target: &str) -> bool {
    let bytes = target.as_bytes();

    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn strip_extra_drive_slash(target: &str) -> &str {
    let bytes = target.as_bytes();

    if bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && matches!(bytes[3], b'/' | b'\\')
    {
        &target[1..]
    } else {
        target
    }
}

#[cfg(test)]
mod link_tests;
