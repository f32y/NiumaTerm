use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, KeyDownEvent, MouseButton, MouseMoveEvent, Role,
    ScrollWheelEvent, Window, canvas, div, fill, point, px, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex};
use rust_i18n::t;

use crate::agent_tab::settings::UI_RADIUS;
use crate::agent_tab::transcript::navigation::Navigation;

pub(super) fn render(
    navigation: &mut Navigation,
    window: &mut Window,
    cx: &mut Context<Navigation>,
) -> AnyElement {
    if navigation.messages.len() < 2 {
        return div().into_any_element();
    }

    let count = navigation.messages.len();
    let has_focus = navigation.focus.is_focused(window);

    let active = navigation
        .keyboard_selection
        .filter(|_| has_focus)
        .or(navigation.hovered)
        .filter(|index| *index < count);

    let current = navigation.current;
    let color = cx.theme().foreground;
    let bounds = navigation.bounds.clone();
    let entity = cx.entity().downgrade();
    let list = navigation.list.clone();
    let viewport_width = f32::from(list.viewport_bounds().size.width);
    let rail_width = (viewport_width * 0.1 - 4.).clamp(12., 24.);
    let preview_width = (viewport_width - 40.).clamp(1., 320.);

    let rail = div()
        .id("agent-message-navigation-rail")
        .debug_selector(|| "agent-message-navigation-rail".into())
        .role(Role::Button)
        .aria_label(t!("agent-navigation-label"))
        .track_focus(&navigation.focus)
        .tab_stop(true)
        .relative()
        .w(px(rail_width))
        .h_full()
        .max_h(px((count.saturating_sub(1) as f32 * 8.).max(16.)))
        .cursor_pointer()
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
            // Layout can repeat a mouse move without pointer movement. Keeping
            // keyboard selection then prevents a repaint from changing its target.
            if event.pressed_button.is_some() || this.pointer == Some(event.position) {
                return;
            }

            this.pointer = Some(event.position);
            this.keyboard_selection = None;

            let next = this.index_at(event.position.y);

            if next != this.hovered {
                this.hovered = next;

                cx.notify();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                this.keyboard_selection = None;

                window.focus(&this.focus, cx);
                cx.stop_propagation();
            }),
        )
        .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
            if let Some(index) = this.index_at(event.position().y) {
                this.select(index, cx);
            }

            cx.stop_propagation();
        }))
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            let count = this.messages.len();

            let selected = this
                .keyboard_selection
                .or(this.hovered)
                .or(this.current)
                .unwrap_or(0)
                .min(count - 1);

            let next = match event.keystroke.key.as_str() {
                "up" => selected.saturating_sub(1),
                "down" => (selected + 1).min(count - 1),
                "home" => 0,
                "end" => count - 1,
                "enter" | "space" => {
                    this.select(selected, cx);
                    cx.stop_propagation();

                    return;
                }
                "escape" => {
                    this.hovered = None;
                    this.keyboard_selection = None;

                    window.blur();

                    cx.notify();
                    cx.stop_propagation();

                    return;
                }
                _ => return,
            };

            this.keyboard_selection = Some(next);

            cx.notify();
            cx.stop_propagation();
        }))
        .child(
            canvas(
                move |area, _, cx| {
                    bounds.set(Some(area));

                    let _ = entity.update(cx, |this, cx| this.observe_scroll(cx));
                },
                move |area, _, window, _| {
                    for index in 0..count {
                        let distance = active.map(|active| active.abs_diff(index));

                        let width: f32 = match distance {
                            Some(0) => 20.,
                            Some(1) => 14.,
                            Some(2) => 10.,
                            _ => 7.,
                        };

                        let opacity = if Some(index) == current {
                            0.85
                        } else if distance == Some(0) {
                            0.65
                        } else {
                            0.25
                        };

                        let y = area.top() + area.size.height * (index as f32 / (count - 1) as f32);

                        window.paint_quad(
                            fill(
                                Bounds::new(
                                    point(area.left() + px(2.), y - px(1.)),
                                    size(px(width.min(rail_width - 4.)), px(2.)),
                                ),
                                color.opacity(opacity),
                            )
                            .corner_radii(px(1.)),
                        );
                    }

                    if has_focus && window.last_input_was_keyboard() {
                        window.paint_quad(fill(
                            Bounds::new(area.origin, size(px(1.), area.size.height)),
                            color.opacity(0.5),
                        ));
                    }
                },
            )
            .size_full(),
        );

    let controls = active
        .or(has_focus.then_some(current.unwrap_or(0)))
        .map(|index| {
            let rail_height = navigation
                .bounds
                .get()
                .map_or(16., |b| f32::from(b.size.height));

            let top = (rail_height * index as f32 / (count - 1) as f32 - 48.)
                .clamp(0., (rail_height - 112.).max(0.));

            div()
                .id("agent-message-navigation-preview")
                .debug_selector(|| "agent-message-navigation-preview".into())
                .absolute()
                .left(px(rail_width + 2.))
                .top(px(top))
                .w(px(preview_width))
                .p_2()
                .rounded(UI_RADIUS)
                .bg(cx.theme().popover)
                .text_color(cx.theme().popover_foreground)
                .shadow_md()
                .text_sm()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            t!(
                                "agent-navigation-position",
                                current = index + 1,
                                total = count
                            )
                            .into_owned(),
                        )
                        .child(
                            h_flex()
                                .child(
                                    Button::new("agent-message-previous")
                                        .ghost()
                                        .small()
                                        .icon(IconName::ChevronUp)
                                        .tooltip(t!("agent-navigation-previous"))
                                        .disabled(index == 0)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.select(index.saturating_sub(1), cx);
                                            cx.stop_propagation();
                                        })),
                                )
                                .child(
                                    Button::new("agent-message-next")
                                        .ghost()
                                        .small()
                                        .icon(IconName::ChevronDown)
                                        .tooltip(t!("agent-navigation-next"))
                                        .disabled(index + 1 == count)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.select(
                                                (index + 1).min(this.messages.len() - 1),
                                                cx,
                                            );

                                            cx.stop_propagation();
                                        })),
                                ),
                        ),
                )
                .child(
                    div()
                        .line_clamp(3)
                        .child(navigation.messages[index].preview.clone()),
                )
        });

    div()
        .absolute()
        .left(px(4.))
        .top(px(48.))
        .bottom(px(48.))
        .w(px(24.))
        .flex()
        .flex_col()
        .justify_center()
        .child(
            div()
                .id("agent-message-navigation")
                .relative()
                .w(px(if active.is_some() || has_focus {
                    rail_width + 2. + preview_width
                } else {
                    rail_width
                }))
                .h_full()
                .max_h(px((count.saturating_sub(1) as f32 * 8.).max(16.)))
                .on_hover(cx.listener(|this, hovered, _, cx| {
                    if !hovered {
                        this.hovered = None;

                        cx.notify();
                    }
                }))
                .on_scroll_wheel(cx.listener(move |_, event: &ScrollWheelEvent, _, cx| {
                    list.scroll_by(-event.delta.pixel_delta(px(24.)).y);

                    cx.notify();
                    cx.stop_propagation();
                }))
                .child(rail)
                .children(controls),
        )
        .into_any_element()
}
