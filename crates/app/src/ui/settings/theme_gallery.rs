use app::design::{
    SPACE_3, SURFACE_RADIUS, THEME_CARD_HEIGHT, THEME_CARD_MIN_WIDTH, THEME_GRID_MAX_COLUMNS,
    THEME_PREVIEW_HEIGHT,
};
use gpui::prelude::*;
use gpui::{App, Div, Entity, Hsla, Rgba, div, px, rgba};
use gpui_base::Button;
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, Sizable as _, h_flex,
    v_flex,
};
use nmt_config::colors::ColorArray;
use nmt_config::theme::{AppearanceTheme, Theme};
use nmt_config::theme_catalog::theme_families;
use rust_i18n::t;

use crate::ui::settings::state::{AppSettings, SettingsEditing};
use crate::ui::settings::theme::select_theme;

fn terminal_color(color: ColorArray) -> Hsla {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;

    rgba(
        channel(color[0]) << 24
            | channel(color[1]) << 16
            | channel(color[2]) << 8
            | channel(color[3]),
    )
    .into()
}

fn preview_color(theme: &Theme, key: &str, fallback: Hsla) -> Hsla {
    theme
        .colors
        .ui
        .as_ref()
        .and_then(|colors| colors.get(key))
        .and_then(|value| value.as_str())
        .and_then(|value| Rgba::try_from(value).ok())
        .map(Into::into)
        .unwrap_or(fallback)
}

/// Identical miniature windows compare chrome, code, and ANSI colors without
/// changing application geometry when the previewed palette changes.
fn theme_preview(theme: &Theme) -> Div {
    let colors = &theme.colors.terminal;
    let surface = terminal_color(colors.background);
    let chrome = preview_color(theme, "background", surface);

    let border = preview_color(
        theme,
        "border",
        terminal_color(colors.foreground).opacity(0.2),
    );

    let text = terminal_color(colors.foreground);

    v_flex()
        .w_full()
        .h(THEME_PREVIEW_HEIGHT)
        .flex_none()
        .rounded(SURFACE_RADIUS)
        .border_1()
        .border_color(border)
        .overflow_hidden()
        .bg(surface)
        .child(
            h_flex()
                .h(px(16.))
                .flex_none()
                .px_2()
                .gap_1()
                .bg(chrome)
                .child(
                    div()
                        .w(px(4.))
                        .h(px(4.))
                        .rounded_full()
                        .bg(text.opacity(0.35)),
                )
                .child(div().w(px(36.)).h(px(8.)).rounded_t(px(3.)).bg(surface)),
        )
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .child(
                    v_flex()
                        .w(px(30.))
                        .h_full()
                        .p_1()
                        .gap_1()
                        .bg(chrome)
                        .child(div().h(px(4.)).w_full().bg(text.opacity(0.16)))
                        .child(div().h(px(4.)).w_full().bg(text.opacity(0.08))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .p_2()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_1()
                                .text_size(px(10.))
                                .overflow_hidden()
                                .child(
                                    div()
                                        .text_color(terminal_color(colors.magenta))
                                        .child("let"),
                                )
                                .child(div().text_color(text).child("theme ="))
                                .child(
                                    div()
                                        .text_color(terminal_color(colors.green))
                                        .child("\"hello\""),
                                ),
                        )
                        .child(
                            h_flex().gap_1().children(
                                [
                                    colors.red,
                                    colors.yellow,
                                    colors.green,
                                    colors.cyan,
                                    colors.blue,
                                    colors.magenta,
                                ]
                                .into_iter()
                                .map(|color| {
                                    div()
                                        .w(px(12.))
                                        .h(px(4.))
                                        .rounded(px(2.))
                                        .bg(terminal_color(color))
                                }),
                            ),
                        ),
                ),
        )
}

fn choose_theme(name: String, editing: &Entity<SettingsEditing>, cx: &mut App) {
    let applied = select_theme(name, cx);

    editing.update(cx, |editing, cx| {
        editing.theme_load_failed = !applied;

        cx.notify();
    });
}

pub(super) fn theme_list(editing: Entity<SettingsEditing>, cx: &mut App) -> Div {
    let selected = cx.global::<AppSettings>().config().theme.clone();
    let state = editing.read(cx);
    let filter = state.theme_filter.to_lowercase();
    let columns = state.theme_columns.max(1);
    let failed = state.theme_load_failed;
    let families = theme_families(state.themes.clone());
    let current = families.iter().find(|family| family.contains(&selected));

    let mode = if cx.theme().mode.is_dark() {
        AppearanceTheme::Dark
    } else {
        AppearanceTheme::Light
    };

    let opposite = if mode == AppearanceTheme::Dark {
        AppearanceTheme::Light
    } else {
        AppearanceTheme::Dark
    };

    let alternate = current
        .filter(|family| family.supports_both_modes())
        .map(|family| family.variant(opposite).id.clone());

    let switch_editing = editing.clone();

    let switch = Switch::new("theme-dark-mode")
        .label(t!("settings-theme-dark-mode").into_owned())
        .small()
        .checked(mode == AppearanceTheme::Dark)
        .disabled(alternate.is_none())
        .on_click(move |_, _, cx| {
            if let Some(name) = &alternate {
                choose_theme(name.clone(), &switch_editing, cx);
            }
        });

    let families = families
        .into_iter()
        .filter(|family| {
            filter.is_empty()
                || family.name.to_lowercase().contains(&filter)
                || family.variants.iter().any(|choice| {
                    choice.id.to_lowercase().contains(&filter)
                        || choice.theme.name.to_lowercase().contains(&filter)
                })
        })
        .collect::<Vec<_>>();

    let border = cx.theme().border;
    let accent = cx.theme().primary;
    let background = cx.theme().popover;
    let hover = cx.theme().secondary;
    let measure = editing.downgrade();

    v_flex()
        .w_full()
        .min_w_0()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap_3()
                .child(t!("settings-theme-title"))
                .child(switch),
        )
        .when(failed, |this| {
            this.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(t!("settings-theme-load-failed")),
            )
        })
        .when(families.is_empty(), |this| {
            this.child(
                div()
                    .py_4()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("settings-theme-no-matches")),
            )
        })
        .child(
            div()
                .w_full()
                .min_w_0()
                .relative()
                .grid()
                .grid_cols(columns)
                .gap(SPACE_3)
                .on_prepaint(move |bounds, window, cx| {
                    let columns = ((bounds.size.width + SPACE_3).as_f32()
                        / (THEME_CARD_MIN_WIDTH + SPACE_3).as_f32())
                    .floor()
                    .clamp(1., f32::from(THEME_GRID_MAX_COLUMNS))
                        as u16;

                    if measure
                        .upgrade()
                        .is_some_and(|editing| editing.read(cx).theme_columns != columns)
                    {
                        // Apply measured geometry after the current layout completes;
                        // a queued frame keeps an otherwise idle window updating.
                        window.on_next_frame(move |_, cx| {
                            let _ = measure.update(cx, |editing, cx| {
                                if editing.theme_columns != columns {
                                    editing.theme_columns = columns;

                                    cx.notify();
                                }
                            });
                        });
                    }
                })
                .children(families.into_iter().enumerate().map(|(index, family)| {
                    let choice = family.variant(mode);
                    let active = family.contains(&selected);
                    let id = choice.id.clone();
                    let target = editing.clone();

                    let name = if family.name.is_empty() {
                        id.clone()
                    } else {
                        family.name.clone()
                    };

                    Button::new(("theme-card", index))
                        .accessibility_label(name.clone())
                        .selected(active)
                        .w_full()
                        .min_w_0()
                        .h(THEME_CARD_HEIGHT)
                        .p_2()
                        .flex_col()
                        .gap_2()
                        .rounded(SURFACE_RADIUS)
                        .border_1()
                        .border_color(if active { accent } else { border })
                        .bg(background)
                        .cursor_pointer()
                        .hover(move |this| this.bg(hover))
                        .focus_visible(move |this| this.border_color(accent))
                        .on_click(move |_, _, cx| choose_theme(id.clone(), &target, cx))
                        .child(theme_preview(&choice.theme))
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_1()
                                .text_size(px(12.))
                                .child(div().flex_1().min_w_0().truncate().child(name))
                                .when(!family.supports_both_modes(), |this| {
                                    this.child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(cx.theme().muted_foreground)
                                            .child(if choice.theme.mode == AppearanceTheme::Dark {
                                                t!("settings-theme-dark-only")
                                            } else {
                                                t!("settings-theme-light-only")
                                            }),
                                    )
                                })
                                .when(active, |this| {
                                    this.child(
                                        Icon::new(IconName::Check).size(px(14.)).text_color(accent),
                                    )
                                }),
                        )
                })),
        )
}
