#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;

use std::time::Duration;

use gpui::prelude::*;
use gpui::{ClipboardItem, Context, ObjectFit, Render, Task, Window, div, img, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::spinner::Spinner;
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use merman::OperationControl;
use rust_i18n::t;

use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::transcript::mermaid::preview::DiagramPreview;
use crate::agent_tab::transcript::mermaid::render::{
    DiagramTheme, RenderedDiagram, rasterize, render,
};
use crate::copy_toast::show_text_copied;

pub(super) struct DiagramView {
    source: String,
    theme: Option<DiagramTheme>,
    scale: f32,
    rendered: Option<RenderedDiagram>,
    error: Option<String>,
    show_source: bool,
    control: OperationControl,
    task: Option<Task<()>>,
    revision: u64,
}

impl DiagramView {
    pub(super) fn new(source: String, cx: &mut Context<Self>) -> Self {
        cx.on_release(|view, cx| {
            view.control.cancel();

            if let Some(rendered) = &view.rendered {
                cx.drop_image(rendered.image.clone(), None);
            }
        })
        .detach();

        Self {
            source,
            theme: None,
            scale: 0.,
            rendered: None,
            error: None,
            show_source: false,
            control: OperationControl::new(),
            task: None,
            revision: 0,
        }
    }

    pub(super) fn source(&self) -> &str {
        &self.source
    }

    pub(super) fn image_bytes(&self) -> usize {
        self.rendered
            .as_ref()
            .and_then(|rendered| rendered.image.as_bytes(0))
            .map_or(0, <[u8]>::len)
    }

    fn start_render(&mut self, theme: DiagramTheme, scale: f32, cx: &mut Context<Self>) {
        let cached = if self.theme.as_ref() == Some(&theme) {
            self.rendered
                .as_ref()
                .map(|value| (value.svg.clone(), value.size))
        } else {
            None
        };

        self.control.cancel();

        let control = OperationControl::new().with_deadline(Duration::from_secs(5));
        let worker_control = control.clone();
        let source = self.source.clone();
        let renderer = cx.svg_renderer();
        let worker_theme = theme.clone();

        self.theme = Some(theme);
        self.scale = scale;
        self.control = control;
        self.error = None;
        self.revision += 1;

        let revision = self.revision;

        self.task = Some(cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    match cached {
                        Some((svg, size)) => rasterize(svg, size, &renderer, scale),
                        None => render(
                            &source,
                            &worker_theme,
                            worker_control.clone(),
                            &renderer,
                            scale,
                        ),
                    }
                    .map_err(|error| format!("{error:#}"))
                })
                .await;

            let _ = view.update(cx, |view, cx| {
                if view.revision != revision {
                    return;
                }

                view.task = None;

                if let Some(old) = view.rendered.take() {
                    cx.drop_image(old.image, None);
                }

                match result {
                    Ok(rendered) => view.rendered = Some(rendered),
                    Err(error) => view.error = Some(error),
                }

                cx.notify();
            });
        }));

        cx.notify();
    }

    fn open_preview(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rendered) = &self.rendered else {
            return;
        };

        let preview = cx.new(|cx| DiagramPreview::new(rendered, window.scale_factor(), cx));

        window.open_dialog(cx, move |dialog, window, _| {
            dialog
                .w(px(1100.).min(window.viewport_size().width * 0.9))
                .child(preview.clone())
        });
    }
}

impl Render for DiagramView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = DiagramTheme::current(cx);
        let scale = window.scale_factor();

        if self.theme.as_ref() != Some(&theme) || self.scale != scale {
            self.start_render(theme, scale, cx);
        }

        let controls = h_flex()
            .gap_1()
            .child(div().flex_1().text_xs().child("Mermaid"))
            .child(
                Button::new("mermaid-copy")
                    .ghost()
                    .xsmall()
                    .label(t!("agent-transcript-copy"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(ClipboardItem::new_string(view.source.clone()));

                        show_text_copied(window, cx);
                    })),
            )
            .when(self.rendered.is_some(), |controls| {
                controls
                    .child(
                        Button::new("mermaid-source-toggle")
                            .ghost()
                            .xsmall()
                            .label(t!(if self.show_source {
                                "agent-mermaid-diagram"
                            } else {
                                "agent-mermaid-source"
                            }))
                            .on_click(cx.listener(|view, _, _, cx| {
                                cx.stop_propagation();

                                view.show_source = !view.show_source;

                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("mermaid-expand")
                            .ghost()
                            .xsmall()
                            .label(t!("agent-mermaid-expand"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                cx.stop_propagation();

                                view.open_preview(window, cx);
                            })),
                    )
            });

        let content = if self.show_source || self.error.is_some() {
            let settings = cx.global::<AgentSettings>();

            div()
                .id("mermaid-source")
                .debug_selector(|| "mermaid-source".into())
                .max_h(px(400.))
                .overflow_y_scroll()
                .font(settings.transcript_font())
                .text_size(px(settings.transcript_font_size))
                .child(TextView::plain("mermaid-source-text", self.source.clone()).selectable(true))
                .into_any_element()
        } else if let Some(rendered) = &self.rendered {
            div()
                .id("mermaid-preview")
                .debug_selector(|| "mermaid-preview".into())
                .w_full()
                .h(px((rendered.size.1 * 1.25).clamp(180., 640.)))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(
                    img(rendered.image.clone())
                        .debug_selector(|| "mermaid-inline-image".into())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
                .on_click(cx.listener(|view, _, window, cx| {
                    cx.stop_propagation();

                    view.open_preview(window, cx);
                }))
                .into_any_element()
        } else {
            div()
                .h(px(100.))
                .flex()
                .items_center()
                .justify_center()
                .child(Spinner::new())
                .into_any_element()
        };

        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(UI_RADIUS)
            .child(controls)
            .when_some(self.error.as_ref(), |view, error| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("{}: {error}", t!("agent-mermaid-error"))),
                )
            })
            .child(content)
    }
}
