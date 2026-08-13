use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Div, Entity, ParentElement as _, SharedString, Styled as _, Subscription,
    Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

/// Persistent input state for a text field inside a profile card, created
/// via `window.use_keyed_state` so it survives the per-frame settings-view
/// rebuild. The subscription writes edits back into the `AppSettings` global.
struct CardInputState {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// A window-keyed text input bound to a value in the `AppSettings` global.
/// `apply` receives the new text on every change. When the backing value
/// changes underneath a reused key (e.g. a profile removal shifts indices),
/// the sync below rewrites the input to match.
pub(super) fn card_text_input(
    key: String,
    value: SharedString,
    masked: bool,
    apply: impl Fn(String, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let state = window.use_keyed_state(SharedString::from(key), cx, {
        let value = value.clone();

        move |window, cx| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(masked)
                    .default_value(value)
            });

            let _subscription = cx.subscribe(&input, move |_, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    apply(value, cx);
                }
            });

            CardInputState {
                input,
                _subscription,
            }
        }
    });

    let input = state.read(cx).input.clone();

    if input.read(cx).value() != value {
        input.update(cx, |input, cx| {
            input.set_value(value.clone(), window, cx);
        });
    }

    input
}

/// One labeled row inside a profile card: title and an optional muted
/// description on the left, with the control on the right. An empty
/// description omits the second line entirely.
pub(super) fn card_row(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl gpui::IntoElement,
    cx: &App,
) -> Div {
    let description = description.into();

    h_flex()
        .w_full()
        .justify_between()
        .items_start()
        .gap_3()
        .child(
            v_flex()
                .flex_1()
                .max_w_3_5()
                .gap_1()
                .child(Label::new(title.into()).text_sm())
                .when(!description.is_empty(), |this| {
                    this.child(
                        gpui::div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(description),
                    )
                }),
        )
        .child(control.into_any_element())
}
