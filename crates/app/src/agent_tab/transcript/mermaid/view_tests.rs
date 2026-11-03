use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, TestAppContext, px};

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::mermaid::render::DiagramTheme;
use crate::agent_tab::transcript::mermaid::store::DiagramStore;
use crate::agent_tab::transcript::mermaid::view::{DiagramUpdate, DiagramView};

#[gpui::test]
async fn recreated_views_reuse_preparation_and_same_height_rasters_do_not_remeasure(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    cx.set_global(AgentSettings::default());

    let store = Arc::new(DiagramStore::default());
    let source = "flowchart TD\nA --> B";
    let first = cx.new(|cx| DiagramView::with_store(source.into(), store.clone(), cx));

    first.update(cx, |view, cx| {
        view.start_render(DiagramTheme::current(cx), 1., cx)
    });

    cx.condition(&first, |view, _| {
        view.rendered.is_some() || view.error.is_some()
    })
    .await;

    let original_svg = first.read_with(cx, |view, _| view.rendered.as_ref().unwrap().svg.clone());
    let weak = first.downgrade();

    drop(first);

    cx.run_until_parked();

    assert!(weak.upgrade().is_none());

    let second = cx.new(|cx| DiagramView::with_store(source.into(), store.clone(), cx));
    let events = Rc::new(RefCell::new(Vec::new()));
    let observed = events.clone();

    let _subscription = cx.update(|cx| {
        cx.subscribe(&second, move |_, event: &DiagramUpdate, _| {
            observed.borrow_mut().push(event.layout_changed)
        })
    });

    second.update(cx, |view, cx| {
        view.start_render(DiagramTheme::current(cx), 1., cx)
    });

    cx.condition(&second, |view, _| {
        view.rendered.is_some() || view.error.is_some()
    })
    .await;

    second.read_with(cx, |view, _| {
        assert!(Arc::ptr_eq(
            &view.rendered.as_ref().unwrap().svg,
            &original_svg
        ))
    });

    assert_eq!(store.preparations(), 1);
    assert_eq!(*events.borrow(), vec![false]);

    second.update(cx, |view, cx| {
        view.start_render(DiagramTheme::current(cx), 2., cx)
    });

    cx.condition(&second, |view, _| view.task.is_none()).await;

    assert_eq!(store.preparations(), 1);
    assert_eq!(*events.borrow(), vec![false, false]);
}

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
