use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Div, Entity, InteractiveElement as _, ParentElement as _, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex};

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

/// The hover hint that carries a label's description. `owner` names the label
/// it belongs to: the hint carries hover state, so it needs an id of its own,
/// and two hints sharing one would share that state.
pub(super) fn description_hint(owner: &str, description: SharedString, cx: &App) -> Stateful<Div> {
    gpui::div()
        .id(SharedString::from(format!("card-hint-{owner}")))
        .flex_shrink_0()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(IconName::CircleHelp).size_3())
        .tooltip(move |window, cx| Tooltip::new(description.clone()).build(window, cx))
}

/// One labeled row inside a profile card: title on the left with the control
/// on the right, and an optional description behind a hover hint beside the
/// title. An empty description omits the hint entirely.
///
/// The description hides behind the hint rather than sitting under the title
/// so that every row is one line tall: the rows stay scannable, the control
/// column keeps one baseline, and a row explaining itself at length costs the
/// page no more height than one that needs no explanation. This is the shape
/// the settings pages built from `SettingItem` already have, and a card is
/// read as one of them.
pub(super) fn card_row(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl gpui::IntoElement,
    cx: &App,
) -> Div {
    let title = title.into();
    let title_id = title.to_string();
    let description = description.into();

    h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .gap_3()
        .child(
            h_flex()
                .flex_1()
                .max_w_3_5()
                .gap_1()
                .items_center()
                .child(Label::new(title).text_sm())
                .when(!description.is_empty(), |this| {
                    this.child(description_hint(&title_id, description, cx))
                }),
        )
        .child(control.into_any_element())
}
