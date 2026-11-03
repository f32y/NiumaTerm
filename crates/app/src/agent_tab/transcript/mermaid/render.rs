#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;

use std::result::Result as StdResult;
use std::sync::Arc;

use anyhow::{Result, bail, ensure};
use gpui::{App, Hsla, ParsedSvg, RenderImage, Rgba, SvgRenderer};
use gpui_component::ActiveTheme as _;
use merman::svg::SvgPipeline;
use merman::{
    MermaidEngine, OperationControl, RenderOutput, RenderRequest, Renderer,
    SourcePresentationConfig, SvgRequest,
};
use quick_xml::Reader;
use quick_xml::events::Event;
use serde_json::json;

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::render::text_style::transcript_highlight_theme;

const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_SVG_BYTES: usize = 8 * 1024 * 1024;
const MAX_RASTER_EDGE: f32 = 2048.;
const MAX_RASTER_PIXELS: f32 = 2_000_000.;

#[derive(Clone, PartialEq)]
pub(super) struct DiagramTheme {
    dark: bool,
    font: String,
    foreground: String,
    background: String,
    fill: String,
    border: String,
}

impl DiagramTheme {
    pub(super) fn current(cx: &App) -> Self {
        let settings = cx.global::<AgentSettings>();

        let background = if settings.pane_background_follows_terminal {
            settings.terminal_background
        } else {
            cx.theme().background
        };

        Self {
            dark: transcript_highlight_theme(cx).appearance.is_dark(),
            font: format!(
                "\"{}\", sans-serif",
                settings
                    .transcript_font()
                    .family
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
            ),
            foreground: css_color(cx.theme().foreground),
            background: css_color(background),
            fill: css_color(cx.theme().muted),
            border: css_color(cx.theme().border),
        }
    }
}

pub(super) struct RenderedDiagram {
    pub(super) svg: Arc<ParsedSvg>,
    pub(super) size: (f32, f32),
    pub(super) image: Arc<RenderImage>,
}

pub(super) fn render_svg(
    source: &str,
    theme: &DiagramTheme,
    control: OperationControl,
) -> Result<String> {
    ensure!(
        source.len() <= MAX_SOURCE_BYTES,
        "Diagram source exceeds 64 KiB"
    );

    let config = SourcePresentationConfig::from_value(json!({
        "theme": "base",
        "darkMode": theme.dark,
        "layout": "dagre",
        "look": "classic",
        "securityLevel": "strict",
        "fontFamily": theme.font,
        "themeVariables": {
            "primaryColor": theme.fill,
            "primaryTextColor": theme.foreground,
            "primaryBorderColor": theme.border,
            "lineColor": theme.foreground,
            "textColor": theme.foreground,
            "mainBkg": theme.fill,
            "background": theme.background,
            "edgeLabelBackground": theme.background,
            "fontFamily": theme.font,
        },
    }));

    let output = Renderer::new()
        .with_presentation_engine(MermaidEngine::new().with_site_config(config))
        .render(RenderRequest::svg(
            source,
            control,
            SvgRequest {
                pipeline: Some(SvgPipeline::resvg_safe()),
                ..Default::default()
            },
        ))?;

    let RenderOutput::Svg(Some(svg)) = output else {
        bail!("No Mermaid diagram was detected");
    };

    let svg = svg.into_parts().0;

    ensure!(svg.len() <= MAX_SVG_BYTES, "Diagram SVG exceeds 8 MiB");

    Ok(svg)
}

pub(super) fn render(
    source: &str,
    theme: &DiagramTheme,
    control: OperationControl,
    renderer: &SvgRenderer,
    scale: f32,
) -> Result<RenderedDiagram> {
    let svg = render_svg(source, theme, control.clone())?;

    control.checkpoint()?;

    let size = svg_size(&svg)?;
    let parsed = Arc::new(renderer.parse_svg(svg.as_bytes())?);

    control.checkpoint()?;

    rasterize(parsed, size, renderer, scale)
}

pub(super) fn rasterize(
    svg: Arc<ParsedSvg>,
    size: (f32, f32),
    renderer: &SvgRenderer,
    scale: f32,
) -> Result<RenderedDiagram> {
    let image = renderer.render_parsed(&svg, raster_scale(size, scale))?;

    Ok(RenderedDiagram { svg, size, image })
}

pub(super) fn raster_scale(size: (f32, f32), scale: f32) -> f32 {
    let (width, height) = size;

    // GPUI doubles scale factors for smoothing. Limit the resulting allocation.
    let scale = (scale * 2.)
        .min(MAX_RASTER_EDGE / width)
        .min(MAX_RASTER_EDGE / height)
        .min((MAX_RASTER_PIXELS / (width * height)).sqrt());

    scale / 2.
}

fn svg_size(svg: &str) -> Result<(f32, f32)> {
    let mut reader = Reader::from_str(svg);

    loop {
        match reader.read_event()? {
            Event::Start(root) | Event::Empty(root) if root.name().as_ref() == "svg" => {
                for attribute in root.attributes() {
                    let attribute = attribute?;

                    if attribute.key.as_ref() == "viewBox" {
                        let value = attribute.value;

                        let values = value
                            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
                            .filter(|part| !part.is_empty())
                            .map(str::parse::<f32>)
                            .collect::<StdResult<Vec<_>, _>>()?;

                        if let [_, _, width, height] = values.as_slice() {
                            ensure!(
                                width.is_finite()
                                    && height.is_finite()
                                    && *width > 0.
                                    && *height > 0.,
                                "Invalid SVG dimensions"
                            );

                            return Ok((*width, *height));
                        }
                    }
                }

                bail!("Diagram SVG has no valid viewBox");
            }
            Event::Eof => bail!("Diagram SVG has no root element"),
            _ => {}
        }
    }
}

fn css_color(color: Hsla) -> String {
    let color = Rgba::from(color);

    format!(
        "#{:02x}{:02x}{:02x}",
        (color.r * 255.).round() as u8,
        (color.g * 255.).round() as u8,
        (color.b * 255.).round() as u8,
    )
}
