use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Entity, FileDialogFilter, IntoElement, ParentElement as _,
    PathPromptOptions, SharedString, Styled as _, Subscription, Window, div, px,
};
use gpui_component::button::Button;
use gpui_component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::setting::{NumberFieldOptions, SettingField};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{ActiveTheme as _, AxisExt as _, Disableable as _, h_flex, v_flex};
use nmt_config::Config;
use nmt_config::appearance::TabShape;
use rust_i18n::t;

use crate::ui::settings::state::AppSettings;
use crate::ui::tab_bar::tab_shape_preview;

#[derive(Clone, Copy)]
enum OpacityTarget {
    Window,
    Image,
}

impl OpacityTarget {
    fn value(self, settings: &AppSettings) -> f64 {
        match self {
            Self::Window => settings.config().appearance.background_opacity,
            Self::Image => settings.config().appearance.background_image_opacity,
        }
    }

    fn min(self) -> f32 {
        match self {
            Self::Window => 0.2,
            Self::Image => 0.0,
        }
    }

    fn set(self, value: f64, settings: &mut AppSettings) {
        settings.edit_appearance(|appearance| match self {
            Self::Window => appearance.background_opacity = value,
            Self::Image => appearance.background_image_opacity = value,
        });
    }
}

struct OpacitySliderState {
    slider: Entity<SliderState>,
    _subscription: Subscription,
}

fn opacity_slider_field(target: OpacityTarget) -> SettingField<SharedString> {
    SettingField::render(move |options, window, cx| {
        let state = window.use_keyed_state(("opacity-slider", target as usize), cx, |_, cx| {
            let value = target.value(cx.global::<AppSettings>()) as f32;

            let slider = cx.new(|_| {
                SliderState::new()
                    .min(target.min())
                    .max(1.0)
                    .step(0.05)
                    .default_value(value)
            });

            let subscription = cx.subscribe(&slider, move |_, _, event: &SliderEvent, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;

                target.set(value.end() as f64, cx.global_mut::<AppSettings>());
            });

            OpacitySliderState {
                slider,
                _subscription: subscription,
            }
        });

        let slider = state.read(cx).slider.clone();

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
                if options.layout().is_horizontal() {
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
                        .disabled(options.is_disabled())
                        .text_color(cx.theme().primary),
                ),
            )
            .child(div().flex_shrink_0().child(format!("{current:.2}")))
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
        let path = cx
            .global::<AppSettings>()
            .config()
            .appearance
            .background_image
            .clone();

        let label: SharedString = path
            .clone()
            .unwrap_or_else(|| t!("settings-common-none").to_string())
            .into();

        h_flex()
            .map(|this| {
                if options.layout().is_horizontal() {
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
                    .label(t!("settings-common-browse"))
                    .disabled(options.is_disabled())
                    .on_click(|_, window, cx| {
                        let rx = cx.prompt_for_paths(PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: Some(t!("settings-background-select-image").into()),
                            file_types: vec![FileDialogFilter {
                                name: t!("settings-background-images-filter").into(),
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
                                        settings.edit_appearance(|section| {
                                            section.background_image = Some(path)
                                        });
                                    });
                                }
                            })
                            .detach();
                    }),
            )
            .children(path.is_some().then(|| {
                Button::new("background-image-clear")
                    .outline()
                    .label(t!("settings-common-clear"))
                    .disabled(options.is_disabled())
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AppSettings>()
                            .edit_appearance(|section| section.background_image = None);
                    })
            }))
    })
}

#[derive(Clone)]
struct TabShapeItem(TabShape);

impl SearchableListItem for TabShapeItem {
    type Value = TabShape;

    /// Read from the catalog on each call, so the rows follow a language
    /// switch made while the picker's state is kept alive.
    fn title(&self) -> SharedString {
        match self.0 {
            TabShape::Rounded => t!("settings-appearance-tab-shape-rounded"),
            TabShape::Attached => t!("settings-appearance-tab-shape-attached"),
        }
        .into()
    }

    fn value(&self) -> &TabShape {
        &self.0
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .py_1()
            .child(self.title())
            .child(tab_shape_preview(self.0, cx))
    }
}

type TabShapeSelectState = SelectState<SearchableVec<TabShapeItem>>;

struct TabShapePicker {
    select: Entity<TabShapeSelectState>,
    _confirm: Subscription,
}

/// Wide enough for the two preview tabs and the rounded strip's gaps beside
/// the row's check mark.
const TAB_SHAPE_MENU_WIDTH: f32 = 280.0;

pub(super) fn tab_shape_field() -> SettingField<SharedString> {
    SettingField::render(|options, window, cx| {
        let picker = window.use_keyed_state("tab-shape-picker", cx, |window, cx| {
            let items = vec![
                TabShapeItem(TabShape::Rounded),
                TabShapeItem(TabShape::Attached),
            ];

            let select = cx.new(|cx| SelectState::new(SearchableVec::new(items), None, window, cx));

            let confirm = cx.subscribe(&select, |_, _, event: &SelectEvent<_>, cx| {
                if let SelectEvent::Confirm(Some(shape)) = event {
                    cx.global_mut::<AppSettings>()
                        .edit_appearance(|section| section.tab_shape = *shape);
                }
            });

            TabShapePicker {
                select,
                _confirm: confirm,
            }
        });

        let select = picker.read(cx).select.clone();

        // A config reload or another window can change the shape while this
        // picker's state lives on, so the selection is reconciled every render.
        let shape = cx.global::<AppSettings>().config().appearance.tab_shape;

        if select.read(cx).selected_value() != Some(&shape) {
            select.update(cx, |state, cx| state.set_selected_value(&shape, window, cx));
        }

        Select::new(&select)
            .menu_width(px(TAB_SHAPE_MENU_WIDTH))
            .when(options.layout().is_vertical(), |this| this.w_full())
    })
}

/// A switch bound to one config value: `read` takes it from the current
/// config and `write` hands a new one to the settings owner, which persists
/// it and applies its side effects.
pub(super) fn settings_switch(
    read: fn(&Config) -> bool,
    write: fn(&mut AppSettings, bool),
) -> SettingField<bool> {
    SettingField::switch(
        move |cx| read(cx.global::<AppSettings>().config()),
        move |value, cx| write(cx.global_mut::<AppSettings>(), value),
    )
}

/// A dropdown bound to one config value stored as a keyed choice: `read`
/// names the current choice's key and `write` hands the picked key to the
/// settings owner, which parses and persists it.
pub(super) fn settings_choice(
    options: Vec<(SharedString, SharedString)>,
    read: fn(&Config) -> &'static str,
    write: fn(&mut AppSettings, &str),
) -> SettingField<SharedString> {
    SettingField::dropdown(
        options,
        move |cx| read(cx.global::<AppSettings>().config()).into(),
        move |value, cx| write(cx.global_mut::<AppSettings>(), value.as_str()),
    )
}

/// A number input bound to one config value, read and written the same way
/// as [`settings_switch`].
pub(super) fn settings_number(
    options: NumberFieldOptions,
    read: fn(&Config) -> f64,
    write: fn(&mut AppSettings, f64),
) -> SettingField<f64> {
    SettingField::number_input(
        options,
        move |cx| read(cx.global::<AppSettings>().config()),
        move |value, cx| write(cx.global_mut::<AppSettings>(), value),
    )
}
