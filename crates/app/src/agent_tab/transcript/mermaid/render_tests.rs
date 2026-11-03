use std::sync::Arc;

use gpui::SvgRenderer;
use merman::OperationControl;

use crate::agent_tab::transcript::mermaid::render::{
    DiagramTheme, MAX_SOURCE_BYTES, rasterize, render, render_svg,
};

impl DiagramTheme {
    fn fixture(dark: bool) -> Self {
        Self {
            dark,
            font: "Segoe UI, Microsoft YaHei, sans-serif".to_owned(),
            foreground: if dark { "#eeeeee" } else { "#111111" }.to_owned(),
            background: if dark { "#161616" } else { "#ffffff" }.to_owned(),
            fill: if dark { "#333333" } else { "#eeeeee" }.to_owned(),
            border: "#888888".to_owned(),
        }
    }
}

#[test]
fn supported_families_generate_images_with_both_themes() {
    let renderer = SvgRenderer::new(Arc::new(()));

    let sources = [
        "flowchart TD\nsubgraph Group\nA[\"开始 中文\"] --> B[\"**Complete**<br/>Second line\"]\nend",
        "sequenceDiagram\nparticipant A as Client\nparticipant B as Server\nA->>B: Request\nalt Found\nB-->>A: Response\nelse Missing\nB-->>A: Error\nend",
        "classDiagram\nclass User {\n+String name\n+login()\n}\nUser --> Session",
        "stateDiagram-v2\n[*] --> Ready\nReady --> Running\nRunning --> [*]",
        "erDiagram\nCUSTOMER ||--o{ ORDER : places\nCUSTOMER {\nstring name\n}",
    ];

    for dark in [false, true] {
        for source in sources {
            let rendered = render(
                source,
                &DiagramTheme::fixture(dark),
                OperationControl::new(),
                &renderer,
                1.,
            )
            .unwrap_or_else(|error| panic!("{source}: {error:#}"));

            let pixels = rendered.image.as_bytes(0).unwrap();

            assert!(!pixels.is_empty());
            assert!(pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0));
            assert!(pixels.len() <= 2_000_000 * 4);

            let enlarged = rasterize(rendered.svg, rendered.size, &renderer, 100.).unwrap();

            assert!(enlarged.image.as_bytes(0).unwrap().len() <= 2_000_000 * 4);
        }
    }
}

#[test]
fn invalid_unsupported_large_and_cancelled_sources_return_errors() {
    let theme = DiagramTheme::fixture(false);

    for source in ["flowchart TD\nA[", "pie\n\"A\": 1"] {
        assert!(render_svg(source, &theme, OperationControl::new()).is_err());
    }

    assert!(
        render_svg(
            &"x".repeat(MAX_SOURCE_BYTES + 1),
            &theme,
            OperationControl::new()
        )
        .is_err()
    );

    let control = OperationControl::new();

    control.cancel();

    assert!(render_svg("flowchart TD\nA --> B", &theme, control).is_err());
}

#[test]
fn native_svg_output_replaces_html_labels() {
    let source = "flowchart TD\nA[\"**Bold**<br/>Another line\"] --> B[Done]";

    let svg = render_svg(
        source,
        &DiagramTheme::fixture(false),
        OperationControl::new(),
    )
    .unwrap();

    assert!(!svg.contains("<foreignObject"));
    assert!(svg.contains("Bold"));
    assert!(svg.contains("Another line"));
}
