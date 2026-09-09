//! How transcript prose and code are styled, from the configured font down to
//! the markdown view every text row is built on.

use std::path::Path;

use gpui::prelude::*;
use gpui::{App, ElementId, Font, SharedString, StyleRefinement, px, rems};
use gpui_component::text::TextViewStyle;
use gpui_component::{ActiveTheme as _, text};

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

pub(crate) fn agent_text_style(cx: &App) -> TextViewStyle {
    // Prose stops at a reading measure. On a maximised window the pane is
    // wide enough for well over a hundred characters a line, and the eye
    // loses the start of the next line on the return sweep. Code blocks
    // and tables stay full-width: their content is scanned column-wise
    // rather than read across, and narrowing them only forces an inner
    // scroll or a wrap that hides structure.
    TextViewStyle::default()
        .prose_max_width(rems(PROSE_MEASURE_REMS))
        .code_block(configured_transcript_code_block_style(cx))
        .table(transcript_table_style(cx))
}

pub(crate) fn work_detail_text_style(cx: &App) -> TextViewStyle {
    TextViewStyle::default()
        .code_block(configured_transcript_code_block_style(cx))
        .table(transcript_table_style(cx))
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
