use gpui::{TestAppContext, px};

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::mermaid::view::DiagramView;

#[gpui::test]
async fn completed_diagram_can_switch_to_source_and_back(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.set_global(AgentSettings::default());

    let (view, cx) = cx
        .add_window_view(|_, cx| DiagramView::new("flowchart TD\nA[Start] --> B[Done]".into(), cx));

    cx.condition(&view, |view, _| {
        view.rendered.is_some() || view.error.is_some()
    })
    .await;

    view.read_with(cx, |view, _| {
        assert!(view.error.is_none(), "{:?}", view.error)
    });

    cx.refresh().unwrap();

    assert!(cx.debug_bounds("mermaid-preview").is_some());

    view.update(cx, |view, cx| {
        view.show_source = true;

        cx.notify();
    });

    cx.refresh().unwrap();

    assert!(cx.debug_bounds("mermaid-source").is_some());
    assert!(cx.debug_bounds("mermaid-preview").is_none());

    view.update(cx, |view, cx| {
        view.show_source = false;

        cx.notify();
    });

    cx.refresh().unwrap();

    assert!(cx.debug_bounds("mermaid-preview").is_some());
}

#[gpui::test]
async fn failed_diagram_displays_source(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.set_global(AgentSettings::default());

    let (view, cx) = cx.add_window_view(|_, cx| DiagramView::new("flowchart TD\nA[".into(), cx));

    cx.condition(&view, |view, _| view.error.is_some()).await;
    cx.refresh().unwrap();

    assert!(cx.debug_bounds("mermaid-source").is_some());
    assert!(cx.debug_bounds("mermaid-preview").is_none());
}

#[gpui::test]
async fn tall_inline_diagrams_use_a_large_centered_image_area(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.set_global(AgentSettings::default());

    let (view, cx) = cx.add_window_view(|_, cx| {
        DiagramView::new(
            "flowchart TD\nA --> B --> C --> D --> E --> F --> G --> H --> I --> J".into(),
            cx,
        )
    });

    cx.condition(&view, |view, _| {
        view.rendered.is_some() || view.error.is_some()
    })
    .await;

    cx.refresh().unwrap();

    let area = cx.debug_bounds("mermaid-preview").unwrap();
    let image = cx.debug_bounds("mermaid-inline-image").unwrap();

    assert!(area.size.height > px(360.));
    assert_eq!(image.size, area.size);
    assert_eq!(image.center(), area.center());
}
