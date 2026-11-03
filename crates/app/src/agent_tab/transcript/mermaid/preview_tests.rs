use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Context, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, Window, point, px, size,
};

use crate::agent_tab::transcript::mermaid::preview::{DiagramPreview, MAX_ZOOM, MIN_ZOOM};
use crate::agent_tab::transcript::mermaid::render::rasterize;

fn preview(window: &mut Window, cx: &mut Context<DiagramPreview>) -> DiagramPreview {
    let renderer = cx.svg_renderer();

    let svg = renderer
        .parse_svg(br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="1200" viewBox="0 0 200 1200"><rect width="200" height="1200" fill="#888"/></svg>"##)
        .unwrap();

    let rendered = rasterize(
        Arc::new(svg),
        (200., 1200.),
        &renderer,
        window.scale_factor(),
    )
    .unwrap();

    DiagramPreview::new(&rendered, window.scale_factor(), cx)
}

#[gpui::test]
fn buttons_zoom_and_reset_the_centered_diagram(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);

    let (view, cx) = cx.add_window_view(preview);

    cx.simulate_resize(size(px(900.), px(400.)));
    cx.run_until_parked();
    cx.refresh().unwrap();

    let viewport = cx.debug_bounds("mermaid-zoom-viewport").unwrap();
    let original = cx.debug_bounds("mermaid-zoom-image").unwrap();

    assert!((original.center().x - viewport.center().x).abs() < px(1.));
    assert!((original.size.height - viewport.size.height).abs() < px(1.));

    let plus = cx.debug_bounds("mermaid-zoom-in").unwrap();

    assert!(plus.bottom() <= viewport.top());

    cx.simulate_click(plus.center(), Modifiers::default());
    cx.refresh().unwrap();

    let enlarged = cx.debug_bounds("mermaid-zoom-image").unwrap();

    assert!(enlarged.size.height > original.size.height);

    let minus = cx.debug_bounds("mermaid-zoom-out").unwrap();

    cx.simulate_click(minus.center(), Modifiers::default());
    cx.refresh().unwrap();

    view.read_with(cx, |view, _| assert!((view.zoom - 1.).abs() < 0.001));

    cx.simulate_click(plus.center(), Modifiers::default());
    cx.refresh().unwrap();

    let reset = cx.debug_bounds("mermaid-zoom-fit").unwrap();

    cx.simulate_click(reset.center(), Modifiers::default());
    cx.refresh().unwrap();

    view.read_with(cx, |view, _| assert_eq!(view.zoom, 1.));

    let reset_image = cx.debug_bounds("mermaid-zoom-image").unwrap();

    assert!((reset_image.center().y - viewport.center().y).abs() < px(1.));
}

#[gpui::test]
async fn wheel_zoom_reuses_the_image_until_input_pauses(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);

    let (view, cx) = cx.add_window_view(preview);

    cx.simulate_resize(size(px(900.), px(400.)));
    cx.run_until_parked();
    cx.refresh().unwrap();

    let viewport = cx.debug_bounds("mermaid-zoom-viewport").unwrap();
    let initial_image = view.read_with(cx, |view, _| view.image.id);

    for _ in 0..3 {
        cx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Lines(point(0., 1.)),
            ..Default::default()
        });
    }

    view.read_with(cx, |view, _| {
        assert!((view.zoom - 1.2_f32.powi(3)).abs() < 0.001);
        assert_eq!(view.image.id, initial_image);
    });

    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.condition(&view, |view, _| view.task.is_none()).await;

    view.read_with(cx, |view, _| {
        assert_ne!(
            view.image.id,
            initial_image,
            "fit={}, zoom={}, image_scale={}, bounds={:?}",
            view.fit_scale(),
            view.zoom,
            view.image_scale,
            view.bounds
        );
    });

    for _ in 0..100 {
        cx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Lines(point(0., 1.)),
            ..Default::default()
        });
    }

    view.read_with(cx, |view, _| assert_eq!(view.zoom, MAX_ZOOM));

    for _ in 0..100 {
        cx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-100.))),
            ..Default::default()
        });
    }

    view.read_with(cx, |view, _| assert_eq!(view.zoom, MIN_ZOOM));
}
