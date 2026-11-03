#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Bounds, Context, MouseButton, MouseDownEvent, MouseMoveEvent, ParsedSvg, Pixels, Point,
    RenderImage, ScrollDelta, ScrollWheelEvent, Size, Task, Window, div, img, point, px, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, ElementExt as _, IconName, Sizable as _, h_flex, v_flex};
use rust_i18n::t;

use crate::agent_tab::transcript::mermaid::render::{RenderedDiagram, raster_scale, rasterize};

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.;
const ZOOM_STEP: f32 = 1.2;
const RASTER_DELAY: Duration = Duration::from_millis(180);

pub(super) struct DiagramPreview {
    svg: Arc<ParsedSvg>,
    natural: (f32, f32),
    image: Arc<RenderImage>,
    owns_image: bool,
    image_scale: f32,
    zoom: f32,
    pan: Point<Pixels>,
    drag: Option<Point<Pixels>>,
    bounds: Option<Bounds<Pixels>>,
    revision: u64,
    task: Option<Task<()>>,
}

impl DiagramPreview {
    pub(super) fn new(rendered: &RenderedDiagram, scale: f32, cx: &mut Context<Self>) -> Self {
        cx.on_release(|view, cx| {
            if view.owns_image {
                cx.drop_image(view.image.clone(), None);
            }
        })
        .detach();

        Self {
            svg: rendered.svg.clone(),
            natural: rendered.size,
            image: rendered.image.clone(),
            owns_image: false,
            image_scale: raster_scale(rendered.size, scale),
            zoom: 1.,
            pan: Point::default(),
            drag: None,
            bounds: None,
            revision: 0,
            task: None,
        }
    }

    fn fit_scale(&self) -> f32 {
        let Some(bounds) = self.bounds else {
            return 1.;
        };

        (f32::from(bounds.size.width) / self.natural.0)
            .min(f32::from(bounds.size.height) / self.natural.1)
    }

    fn display_size(&self) -> Size<Pixels> {
        let scale = self.fit_scale() * self.zoom;

        size(px(self.natural.0 * scale), px(self.natural.1 * scale))
    }

    fn clamp_pan(&mut self) {
        let Some(bounds) = self.bounds else {
            return;
        };

        let shown = self.display_size();
        let limit_x = ((shown.width - bounds.size.width) / 2.).max(px(0.));
        let limit_y = ((shown.height - bounds.size.height) / 2.).max(px(0.));

        self.pan.x = self.pan.x.clamp(-limit_x, limit_x);
        self.pan.y = self.pan.y.clamp(-limit_y, limit_y);
    }

    fn set_zoom(
        &mut self,
        zoom: f32,
        anchor: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);

        if (zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }

        self.pan = anchor - (anchor - self.pan) * (zoom / self.zoom);
        self.zoom = zoom;

        self.clamp_pan();
        self.schedule_raster(window.scale_factor(), cx);

        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();

        let steps = match event.delta {
            ScrollDelta::Lines(delta) => delta.y.clamp(-1., 1.),
            ScrollDelta::Pixels(delta) => (f32::from(delta.y) / 100.).clamp(-1., 1.),
        };

        let anchor = self
            .bounds
            .map_or(Point::default(), |bounds| event.position - bounds.center());

        self.set_zoom(self.zoom * ZOOM_STEP.powf(steps), anchor, window, cx);
    }

    fn on_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;

            return;
        }

        if let Some(previous) = self.drag {
            self.drag = Some(event.position);
            self.pan += event.position - previous;

            self.clamp_pan();

            cx.notify();
        }
    }

    fn schedule_raster(&mut self, display_scale: f32, cx: &mut Context<Self>) {
        let scale = self.fit_scale() * self.zoom * display_scale;
        let target = raster_scale(self.natural, scale);

        self.revision += 1;
        self.task = None;

        if (target - self.image_scale).abs() < 0.001 {
            return;
        }

        let revision = self.revision;
        let svg = self.svg.clone();
        let natural = self.natural;
        let renderer = cx.svg_renderer();

        // Scale the existing image during input, then update its resolution once
        // input pauses. Parsed SVG reuse avoids repeating Mermaid layout.
        self.task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor().timer(RASTER_DELAY).await;

            let result = cx
                .background_spawn(async move { rasterize(svg, natural, &renderer, scale) })
                .await;

            let _ = view.update(cx, |view, cx| {
                if view.revision != revision {
                    return;
                }

                view.task = None;

                if let Ok(rendered) = result {
                    if view.owns_image {
                        cx.drop_image(view.image.clone(), None);
                    }

                    view.image = rendered.image;
                    view.owns_image = true;
                    view.image_scale = target;

                    cx.notify();
                }
            });
        }));
    }
}

impl Render for DiagramPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.display_size();
        let available = self.bounds.map_or(Size::default(), |bounds| bounds.size);

        let origin = point(
            (available.width - shown.width) / 2. + self.pan.x,
            (available.height - shown.height) / 2. + self.pan.y,
        );

        let owner = cx.entity().downgrade();

        let controls = h_flex()
            .w_full()
            .pr_8()
            .gap_1()
            .child(div().flex_1().text_lg().child("Mermaid"))
            .child(
                Button::new("mermaid-zoom-out")
                    .debug_selector(|| "mermaid-zoom-out".into())
                    .ghost()
                    .small()
                    .icon(IconName::Minus)
                    .tooltip(t!("agent-mermaid-zoom-out"))
                    .disabled(self.zoom <= MIN_ZOOM)
                    .on_click(cx.listener(|view, _, window, cx| {
                        cx.stop_propagation();

                        view.set_zoom(view.zoom / ZOOM_STEP, Point::default(), window, cx);
                    })),
            )
            .child(
                Button::new("mermaid-zoom-fit")
                    .debug_selector(|| "mermaid-zoom-fit".into())
                    .ghost()
                    .small()
                    .label(format!("{:.0}%", self.zoom * 100.))
                    .tooltip(t!("agent-mermaid-zoom-fit"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        cx.stop_propagation();

                        view.pan = Point::default();

                        view.set_zoom(1., Point::default(), window, cx);

                        cx.notify();
                    })),
            )
            .child(
                Button::new("mermaid-zoom-in")
                    .debug_selector(|| "mermaid-zoom-in".into())
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .tooltip(t!("agent-mermaid-zoom-in"))
                    .disabled(self.zoom >= MAX_ZOOM)
                    .on_click(cx.listener(|view, _, window, cx| {
                        cx.stop_propagation();

                        view.set_zoom(view.zoom * ZOOM_STEP, Point::default(), window, cx);
                    })),
            );

        v_flex().w_full().gap_2().child(controls).child(
            div()
                .id("mermaid-zoom-viewport")
                .debug_selector(|| "mermaid-zoom-viewport".into())
                .relative()
                .w_full()
                .h(window.viewport_size().height * 0.7)
                .overflow_hidden()
                .cursor_move()
                .on_prepaint(move |bounds, window, cx| {
                    let _ = owner.update(cx, |view, cx| {
                        let changed = view.bounds.is_none_or(|old| old.size != bounds.size);

                        view.bounds = Some(bounds);

                        if changed {
                            view.clamp_pan();
                            view.schedule_raster(window.scale_factor(), cx);

                            cx.notify();
                        }
                    });
                })
                .on_scroll_wheel(cx.listener(Self::on_scroll))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();

                        view.drag = Some(event.position);
                    }),
                )
                .on_mouse_move(cx.listener(|view, event, _, cx| view.on_move(event, cx)))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|view, _, _, _| view.drag = None),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|view, _, _, _| view.drag = None),
                )
                .child(
                    img(self.image.clone())
                        .debug_selector(|| "mermaid-zoom-image".into())
                        .absolute()
                        .left(origin.x)
                        .top(origin.y)
                        .w(shown.width)
                        .h(shown.height),
                ),
        )
    }
}
