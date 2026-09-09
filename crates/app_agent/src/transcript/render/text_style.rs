//! How transcript prose and code are styled, from the configured font down to
//! the markdown view every text row is built on.

use std::path::Path;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, ElementId, Font, Hsla, SharedString, StyleRefinement, px, rems};
use gpui_component::highlighter::HighlightTheme;
use gpui_component::text::TextViewStyle;
use gpui_component::{ActiveTheme as _, text};
use nmt_app_terminal::frame::theme_default_background;

use crate::links;
use crate::settings::AgentSettings;
use crate::transcript::render::PROSE_MEASURE_REMS;

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

    let surface: Hsla = gpui::rgb(theme_default_background().rgb_u32()).into();

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

pub(crate) fn agent_text_style(cx: &App) -> TextViewStyle {
    // Prose stops at a reading measure. On a maximised window the pane is
    // wide enough for well over a hundred characters a line, and the eye
    // loses the start of the next line on the return sweep. Code blocks
    // and tables stay full-width: their content is scanned column-wise
    // rather than read across, and narrowing them only forces an inner
    // scroll or a wrap that hides structure.
    let mut style = TextViewStyle::default()
        .prose_max_width(rems(PROSE_MEASURE_REMS))
        .code_block(configured_transcript_code_block_style(cx))
        .table(transcript_table_style(cx));
    style.highlight_theme = transcript_highlight_theme(cx);
    style
}

pub(crate) fn work_detail_text_style(cx: &App) -> TextViewStyle {
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
        links::open(target, cwd.as_deref().map(Path::new), cx);
    })
}
