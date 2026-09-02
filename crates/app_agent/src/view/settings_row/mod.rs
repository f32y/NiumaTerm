pub(super) mod effort;
mod harness_rows;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Div, IntoElement, Pixels, SharedString, Stateful, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{ActiveTheme as _, Icon, IconName, IconNamed, Sizable as _, h_flex};
use nmt_i18n::i18n;

use crate::AgentPane;
use crate::commands::setting_value_label;
use crate::profile::AgentKind;
use crate::settings::AgentSettings;
use crate::view::settings_row::effort::EffortGaugeIcon;

/// One composer setting, drawn as its own pill. Each pill opens its own menu
/// and changes one value, so each carries its own outline: a shared frame
/// around several of them reads as a segmented control whose parts move
/// together, which is the opposite of what these do.
const SETTINGS_PILL_RADIUS: f32 = 8.0;
const SETTINGS_PILL_PADDING_X: f32 = 9.0;
const SETTINGS_PILL_GAP: f32 = 6.0;
const SETTINGS_PILL_TEXT: f32 = 13.0;
const SETTINGS_PILL_ICON: f32 = 12.0;
/// The disclosure mark is the quietest thing on a pill: it says the value can
/// be changed, while the value itself is what the user came to read.
const SETTINGS_PILL_CHEVRON: f32 = 10.0;

/// Faces the effort gauge is drawn with, past the empty one. Six is what the
/// longest ladder any harness offers needs, so every level of every ladder
/// lands on a face of its own.
const EFFORT_GAUGE_STEPS: usize = 6;

impl IconNamed for EffortGaugeIcon {
    fn path(self) -> SharedString {
        format!("icons/effort-gauge-{}.svg", self.0.min(EFFORT_GAUGE_STEPS)).into()
    }
}

/// One setting the composer row keeps off its surface, as the menu behind the
/// row needs it: what it is called, what it stands at, what it could stand at,
/// and how to move it.
///
/// The model and the effort are what a user changes between one message and
/// the next. The rest are a deployment's standing choices, read once and left
/// alone, and a pill each for them crowds the two that are actually read.
#[derive(Clone)]
pub(super) struct FoldedSetting {
    name: &'static str,
    icon: IconName,
    current: Option<String>,
    options: Vec<(String, String)>,
    set: fn(&mut AgentPane, String, &mut Context<AgentPane>),
}

impl AgentPane {
    /// The dropdown row under the input, per agent kind.
    pub(super) fn render_settings_row(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.kind {
            AgentKind::Codex => self.render_codex_settings_row(cx).into_any_element(),
            AgentKind::Claude => self.render_claude_settings_row(cx).into_any_element(),
            AgentKind::DeepSeek => self.render_deepseek_settings_row(cx).into_any_element(),
        }
    }

    /// The effort ladder the composer offers, cheapest first. It is this
    /// application's rather than the harness's: Codex reports no per-model
    /// levels at all, so reading them from the session spread its whole
    /// serialization range across the control, including values no model
    /// answers to.
    const EFFORT_LEVELS: [&'static str; 5] = ["low", "medium", "high", "xhigh", "max"];

    /// The model catalog as picker entries, spelled the way the settings ask
    /// for. A catalog entry carries both names of one model - the one the
    /// harness displays and the route id a pick is sent as - and which of them
    /// tells the user what they are choosing depends on the deployment, so the
    /// pairing is a setting rather than a decision made here.
    fn model_options(&self, cx: &App) -> Vec<(String, String)> {
        let style = cx.global::<AgentSettings>().model_list_style;

        self.controls
            .models
            .iter()
            .map(|m| (m.model.clone(), style.label(&m.display, &m.model)))
            .collect()
    }

    /// The control the folded settings live behind: one menu listing them by
    /// name and by the value each stands at, with a submenu per setting for
    /// the values it could stand at instead.
    ///
    /// Nothing to fold means no control, so a harness offering only a model
    /// and an effort keeps a row of two rather than one that ends in an empty
    /// menu.
    fn folded_settings_pill(
        cx: &mut Context<Self>,
        settings: Vec<FoldedSetting>,
    ) -> Option<AnyElement> {
        if settings.is_empty() {
            return None;
        }

        let pane = cx.entity();
        let name = i18n("agent-settings-folded");

        let pill = Self::settings_pill(Button::new("agent-folded-settings"))
            .tooltip(name)
            .aria_label(name)
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(IconName::Ellipsis)
                            .size(px(SETTINGS_PILL_ICON))
                            .text_color(cx.theme().muted_foreground.opacity(0.8)),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(SETTINGS_PILL_CHEVRON))
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    ),
            )
            // Anchored bottom-left so the menu opens upward — the row sits
            // at the bottom edge of the pane.
            .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, window, cx| {
                let mut menu = menu;

                for setting in settings.clone() {
                    // The entry states the value as well as the name, so
                    // what the row used to show on its surface is still
                    // read without opening anything further.
                    let value = setting
                        .current
                        .as_ref()
                        .map(|value| {
                            setting
                                .options
                                .iter()
                                .find(|(option, _)| option == value)
                                .map(|(_, label)| label.clone())
                                .unwrap_or_else(|| setting_value_label(value))
                        })
                        .unwrap_or_else(|| "—".to_string());
                    let label = i18n("agent-settings-folded-entry")
                        .replace("{name}", setting.name)
                        .replace("{value}", &value);
                    let pane = pane.clone();

                    menu = menu.submenu_with_icon(
                        Some(Icon::new(setting.icon)),
                        label,
                        window,
                        cx,
                        move |submenu, _, _| {
                            let mut submenu = submenu;
                            let set = setting.set;

                            for (value, label) in setting.options.clone() {
                                let pane = pane.clone();
                                let checked = setting.current.as_deref() == Some(value.as_str());

                                submenu = submenu.item(
                                    PopupMenuItem::new(label).checked(checked).on_click(
                                        move |_, _, cx| {
                                            pane.update(cx, |this, cx| {
                                                set(this, value.clone(), cx);
                                                cx.notify();
                                            });
                                        },
                                    ),
                                );
                            }

                            submenu
                        },
                    );
                }

                menu
            });

        Some(Self::settings_pill_frame(pill, cx).into_any_element())
    }

    /// The pills that belong to one aspect of the thread, named for assistive
    /// technology. Purely a grouping: the pills inside it are spaced exactly
    /// like the pills on either side of it, so the row reads as one line of
    /// independent settings.
    fn settings_group(label: &'static str, controls: Vec<AnyElement>) -> Stateful<Div> {
        h_flex()
            .id(label)
            .aria_label(label)
            .gap(px(SETTINGS_PILL_GAP))
            .children(controls)
    }

    /// The corner and inner spacing every composer pill shares.
    fn settings_pill(button: Button) -> Button {
        button
            .ghost()
            .small()
            .rounded(px(SETTINGS_PILL_RADIUS))
            .px(px(SETTINGS_PILL_PADDING_X))
    }

    /// The box a pill's outline is drawn on, which shows it only under the
    /// pointer. At rest the row reads as a line of values rather than a line
    /// of boxes, which keeps it quieter than the prompt above it; the outline
    /// appears where the pointer is, to say the value under it opens.
    ///
    /// It has to be a box around the pill rather than the pill itself. A
    /// Button sets a hover style of its own while it renders, gpui keeps one
    /// per element, and that one resolves a ghost button's border to
    /// transparent — so an outline hung on the pill's own hover is the outline
    /// the Button then paints away.
    fn settings_pill_frame(pill: impl IntoElement, cx: &App) -> Div {
        let border = cx.theme().border;

        div()
            .flex_none()
            .rounded(px(SETTINGS_PILL_RADIUS))
            .border_1()
            .border_color(cx.theme().transparent)
            .hover(move |style| style.border_color(border))
            .child(pill)
    }

    /// Height of the effort track, and the inset its thumb keeps from the
    /// track's edge.
    const EFFORT_TRACK_HEIGHT: Pixels = px(26.0);
    const EFFORT_THUMB_INSET: Pixels = px(3.0);

    /// One dropdown showing `icon · current value · chevron`. The model is the
    /// only setting still shown this way, so the picker carries the floor that
    /// keeps a route id readable rather than taking it as an argument. Menus
    /// keep the existing protocol values and setters.
    fn setting_picker(
        cx: &mut Context<Self>,
        id: &'static str,
        name: &'static str,
        icon: IconName,
        current: Option<String>,
        options: Vec<(String, String)>,
        set: fn(&mut Self, String, &mut Context<Self>),
    ) -> impl IntoElement + use<> {
        let pane = cx.entity();

        // Show the display label of the current protocol value when we know it.
        let current_label = current
            .as_ref()
            .map(|value| {
                options
                    .iter()
                    .find(|(option_value, _)| option_value == value)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| setting_value_label(value))
            })
            .unwrap_or_else(|| "—".to_string());

        let pill = Self::settings_pill(Button::new(id))
            .min_w(px(120.))
            .tooltip(name)
            .aria_label(format!("{name}: {current_label}"))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(icon)
                            .size(px(SETTINGS_PILL_ICON))
                            .text_color(cx.theme().muted_foreground.opacity(0.8)),
                    )
                    .child(div().text_size(px(SETTINGS_PILL_TEXT)).child(current_label))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(SETTINGS_PILL_CHEVRON))
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    ),
            )
            // Anchored bottom-left so the menu opens upward — the row sits at
            // the bottom edge of the pane.
            .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
                let mut menu = menu;

                if options.is_empty() {
                    menu = menu.label(i18n("agent-setting-loading"));
                }

                for (value, label) in options.clone() {
                    let pane = pane.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        pane.update(cx, |this, cx| {
                            set(this, value.clone(), cx);
                            cx.notify();
                        });
                    }));
                }

                menu
            });

        Self::settings_pill_frame(pill, cx)
    }
}
