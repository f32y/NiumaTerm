use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Entity, FileDialogFilter, Global, ParentElement as _, PathPromptOptions,
    SharedString, Styled as _, Subscription, div,
};
use gpui_component::button::Button;
use gpui_component::setting::SettingField;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{ActiveTheme as _, AxisExt as _, Disableable as _, h_flex};

use super::state::{AppSettings, clamp_background_image_opacity, clamp_background_opacity};

#[derive(Clone, Copy)]
enum OpacityTarget {
    Window,
    Image,
}

impl OpacityTarget {
    fn value(self, settings: &AppSettings) -> f64 {
        match self {
            Self::Window => settings.background_opacity,
            Self::Image => settings.background_image_opacity,
        }
    }

    fn min(self) -> f32 {
        match self {
            Self::Window => 0.2,
            Self::Image => 0.0,
        }
    }

    fn set(self, value: f64, settings: &mut AppSettings) {
        match self {
            Self::Window => settings.background_opacity = clamp_background_opacity(value),
            Self::Image => {
                settings.background_image_opacity = clamp_background_image_opacity(value)
            }
        }
    }
}

/// Both opacity fields share persistent slider entities because the settings
/// view and its field closures are rebuilt every render.
struct OpacitySliderState {
    window: Entity<SliderState>,
    image: Entity<SliderState>,
    _subscriptions: [Subscription; 2],
}

impl Global for OpacitySliderState {}

fn opacity_slider_field(target: OpacityTarget) -> SettingField<SharedString> {
    SettingField::render(move |options, window, cx| {
        if !cx.has_global::<OpacitySliderState>() {
            let make_slider = |target: OpacityTarget, cx: &mut App| {
                let value = target.value(cx.global::<AppSettings>()) as f32;
                let slider = cx.new(|_| {
                    SliderState::new()
                        .min(target.min())
                        .max(1.0)
                        .step(0.05)
                        .default_value(value)
                });

                let subscription = cx.subscribe(&slider, move |_, event: &SliderEvent, cx| {
                    let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                    target.set(value.end() as f64, cx.global_mut::<AppSettings>());
                });

                (slider, subscription)
            };

            let (window_slider, window_subscription) = make_slider(OpacityTarget::Window, cx);
            let (image_slider, image_subscription) = make_slider(OpacityTarget::Image, cx);

            cx.set_global(OpacitySliderState {
                window: window_slider,
                image: image_slider,
                _subscriptions: [window_subscription, image_subscription],
            });
        }

        let sliders = cx.global::<OpacitySliderState>();
        let slider = match target {
            OpacityTarget::Window => &sliders.window,
            OpacityTarget::Image => &sliders.image,
        }
        .clone();

        let current = target.value(cx.global::<AppSettings>()) as f32;

        if (slider.read(cx).value().end() - current).abs() > 0.001 {
            slider.update(cx, |state, cx| state.set_value(current, window, cx));
        }

        h_flex()
            // The setting row's field slot is auto-sized, so a percentage
            // width resolves to the content width (zero for the slider bar)
            // and the whole control collapses; horizontal layout needs a
            // fixed width, like NumberField's `w_32`.
            .map(|this| {
                if options.layout.is_horizontal() {
                    this.w_56()
                } else {
                    this.w_full()
                }
            })
            .gap_2()
            // The thumb (16px, centered on the track position) overhangs the
            // track by 8px at either end; pad so it stays inside the setting
            // row's overflow_hidden instead of being clipped at min/max.
            //
            // Thumb color: the dark theme leaves `slider.thumb` unset and its
            // `primary_foreground` fallback (neutral-900) vanishes against the
            // neutral-950 panel, so use `primary`, which contrasts with the
            // panel in both modes.
            .child(
                div().flex_1().px_2().child(
                    Slider::new(&slider)
                        .disabled(options.disabled)
                        .text_color(cx.theme().primary),
                ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .child(SharedString::from(format!("{current:.2}"))),
            )
    })
}

pub(super) fn background_opacity_field() -> SettingField<SharedString> {
    opacity_slider_field(OpacityTarget::Window)
}

pub(super) fn background_image_opacity_field() -> SettingField<SharedString> {
    opacity_slider_field(OpacityTarget::Image)
}

pub(super) fn background_image_field() -> SettingField<SharedString> {
    SettingField::render(|options, _window, cx| {
        let path = cx.global::<AppSettings>().background_image.clone();
        let label = SharedString::from(path.clone().unwrap_or_else(|| "None".to_string()));

        h_flex()
            .map(|this| {
                if options.layout.is_horizontal() {
                    this.w_64()
                } else {
                    this.w_full()
                }
            })
            .gap_2()
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                Button::new("background-image-browse")
                    .outline()
                    .label("Browse")
                    .disabled(options.disabled)
                    .on_click(|_, window, cx| {
                        let rx = cx.prompt_for_paths(PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: Some("Select background image".into()),
                            file_types: vec![FileDialogFilter {
                                name: "Images".into(),
                                extensions: ["png", "jpg", "jpeg", "webp", "bmp"]
                                    .into_iter()
                                    .map(Into::into)
                                    .collect(),
                            }],
                        });
                        window
                            .spawn(cx, async move |cx| {
                                if let Ok(Ok(Some(paths))) = rx.await
                                    && let Some(path) = paths.first()
                                {
                                    let path = path.display().to_string();
                                    let _ = cx.update_global(|settings: &mut AppSettings, _, _| {
                                        settings.background_image = Some(path);
                                    });
                                }
                            })
                            .detach();
                    }),
            )
            .children(path.is_some().then(|| {
                Button::new("background-image-clear")
                    .outline()
                    .label("Clear")
                    .disabled(options.disabled)
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AppSettings>().background_image = None;
                    })
            }))
    })
}
