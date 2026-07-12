use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Entity, IntoElement, SharedString, StyleRefinement, Styled,
    Subscription, Window,
};

use crate::input::{InputEvent, InputState, NumberInput, NumberInputEvent, StepAction};
use crate::setting::fields::{SettingFieldRender, get_value, set_value};
use crate::setting::{AnySettingField, RenderOptions};
use crate::{AxisExt, Disableable, Sizable, StyledExt};

#[derive(Clone, Debug)]
pub struct NumberFieldOptions {
    /// The minimum value for the number input, default is `f64::MIN`.
    pub min: f64,
    /// The maximum value for the number input, default is `f64::MAX`.
    pub max: f64,
    /// The step value for the number input, default is `1.0`.
    pub step: f64,
}

impl Default for NumberFieldOptions {
    fn default() -> Self {
        Self {
            min: f64::MIN,
            max: f64::MAX,
            step: 1.0,
        }
    }
}

pub(crate) struct NumberField {
    options: NumberFieldOptions,
}

impl NumberField {
    pub(crate) fn new(options: Option<&NumberFieldOptions>) -> Self {
        Self {
            options: options.cloned().unwrap_or_default(),
        }
    }
}

struct State {
    input: Entity<InputState>,
    initial_value: f64,
    _subscriptions: Vec<Subscription>,
}

fn accepted_change_value(value: f64, options: &NumberFieldOptions) -> Option<f64> {
    if value < options.min || value > options.max {
        None
    } else {
        Some(value)
    }
}

impl SettingFieldRender for NumberField {
    fn render(
        &self,
        field: Rc<dyn AnySettingField>,
        options: &RenderOptions,
        style: &StyleRefinement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let value = get_value::<f64>(&field, cx);
        let set_value = set_value::<f64>(&field, cx);
        let step_set_value = set_value.clone();
        let num_options = self.options.clone();

        let state_entity = window.use_keyed_state(
            SharedString::from(format!(
                "number-state-{}-{}-{}",
                options.page_ix, options.group_ix, options.item_ix
            )),
            cx,
            |window, cx| {
                // Configure the InputState with the field's step/min/max so
                // the built-in +/- stepping uses them (it defaults to step 1
                // and would otherwise never emit NumberInputEvent::Step).
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(value.to_string())
                        .step(num_options.step)
                        .min(num_options.min)
                        .max(num_options.max)
                });
                let _subscriptions = vec![
                    cx.subscribe_in(&input, window, {
                        move |state: &mut State, input, event: &NumberInputEvent, window, cx| {
                            match event {
                                NumberInputEvent::Step(action) => {
                                    let value = input.read(cx).value();
                                    if let Ok(value) = value.parse::<f64>() {
                                        let new_value = if *action == StepAction::Increment {
                                            value + num_options.step
                                        } else {
                                            value - num_options.step
                                        };
                                        let clamp_value =
                                            new_value.clamp(num_options.min, num_options.max);

                                        input.update(cx, |input, cx| {
                                            input.set_value(
                                                SharedString::from(clamp_value.to_string()),
                                                window,
                                                cx,
                                            );
                                        });
                                        step_set_value(clamp_value, cx);
                                        state.initial_value = clamp_value;
                                    }
                                }
                            }
                        }
                    }),
                    cx.subscribe_in(&input, window, {
                        move |state: &mut State, input, event: &InputEvent, _window, cx| match event
                        {
                            InputEvent::Change => {
                                input.update(cx, |input, cx| {
                                    let value = input.value();
                                    if value == state.initial_value.to_string() {
                                        return;
                                    }

                                    if let Ok(value) = value.parse::<f64>() {
                                        if let Some(value) =
                                            accepted_change_value(value, &num_options)
                                        {
                                            set_value(value, cx);
                                            state.initial_value = value;
                                        }
                                    }
                                });
                            }
                            _ => {}
                        }
                    }),
                ];

                State {
                    input,
                    initial_value: value,
                    _subscriptions,
                }
            },
        );

        // Sync the displayed value when the underlying setting changed externally
        state_entity.update(cx, |state, cx| {
            if state.initial_value != value {
                state.initial_value = value;
                state.input.update(cx, |input, cx| {
                    input.set_value(SharedString::from(value.to_string()), window, cx);
                });
            }
        });

        let state = state_entity.read(cx);

        NumberInput::new(&state.input)
            .disabled(options.disabled)
            .with_size(options.size)
            .map(|this| {
                if options.layout.is_horizontal() {
                    this.w_32()
                } else {
                    this.w_full()
                }
            })
            .refine_style(style)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_change_keeps_out_of_range_text_editable() {
        let options = NumberFieldOptions {
            min: 6.0,
            max: 72.0,
            step: 0.1,
        };

        assert_eq!(accepted_change_value(1.0, &options), None);
        assert_eq!(accepted_change_value(16.0, &options), Some(16.0));
        assert_eq!(accepted_change_value(100.0, &options), None);
    }
}
