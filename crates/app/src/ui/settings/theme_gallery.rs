use app::design::{
    SPACE_3, SURFACE_RADIUS, THEME_CARD_HEIGHT, THEME_CARD_MIN_WIDTH, THEME_GRID_MAX_COLUMNS,
    THEME_PREVIEW_HEIGHT,
};
use gpui::prelude::*;
use gpui::{
    App, Bounds, ContentMask, Corners, Div, Entity, Hsla, Rgba, TextAlign, TextRun, canvas, div,
    fill, point, px, rgba, size,
};
use gpui_base::Button;
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, Sizable as _, h_flex,
    v_flex,
};
use nmt_config::colors::ColorArray;
use nmt_config::theme::{AppearanceTheme, Theme};
use nmt_config::theme_catalog::ThemeFamily;
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

    let snippets = [
        ("let", terminal_color(colors.magenta)),
        ("theme =", text),
        ("\"hello\"", terminal_color(colors.green)),
    ];

    let ansi = [
        colors.red,
        colors.yellow,
        colors.green,
        colors.cyan,
        colors.blue,
        colors.magenta,
    ]
    .map(terminal_color);

    div()
        .w_full()
        .h(THEME_PREVIEW_HEIGHT)
        .flex_none()
        .rounded(SURFACE_RADIUS)
        .border_1()
        .border_color(border)
        .overflow_hidden()
        .bg(surface)
        // Preview geometry is fixed decoration. Direct drawing avoids flex
        // layout for dozens of tiny boxes in every visible card.
        .child(
            canvas(
                move |_, window, _| {
                    let mut style = window.text_style();

                    style.font_size = px(10.).into();

                    let font = style.font();

                    let lines = snippets.map(|(label, color)| {
                        window.text_system().shape_line(
                            label.into(),
                            px(10.),
                            &[TextRun {
                                len: label.len(),
                                font: font.clone(),
                                color,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            }],
                            None,
                        )
                    });

                    (lines, style.line_height_in_pixels(window.rem_size()))
                },
                move |bounds, (lines, line_height), window, cx| {
                    let rect = |x, y, width, height| {
                        Bounds::new(bounds.origin + point(x, y), size(width, height))
                    };

                    window.paint_quad(fill(
                        rect(px(0.), px(0.), bounds.size.width, px(16.)),
                        chrome,
                    ));

                    window.paint_quad(
                        fill(rect(px(8.), px(6.), px(4.), px(4.)), text.opacity(0.35))
                            .corner_radii(px(2.)),
                    );

                    window.paint_quad(
                        fill(rect(px(16.), px(4.), px(36.), px(8.)), surface).corner_radii(
                            Corners {
                                top_left: px(3.),
                                top_right: px(3.),
                                ..Default::default()
                            },
                        ),
                    );

                    window.paint_quad(fill(
                        rect(
                            px(0.),
                            px(16.),
                            px(30.),
                            (bounds.size.height - px(16.)).max(px(0.)),
                        ),
                        chrome,
                    ));

                    for (y, opacity) in [(20., 0.16), (28., 0.08)] {
                        window.paint_quad(fill(
                            rect(px(4.), px(y), px(22.), px(4.)),
                            text.opacity(opacity),
                        ));
                    }

                    let code_bounds = rect(
                        px(38.),
                        px(24.),
                        (bounds.size.width - px(46.)).max(px(0.)),
                        line_height,
                    );

                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: code_bounds,
                        }),
                        |window| {
                            let mut origin = code_bounds.origin;

                            for line in lines {
                                let _ = line.paint(
                                    origin,
                                    line_height,
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );

                                origin.x += line.width + px(4.);
                            }
                        },
                    );

                    for (index, color) in ansi.into_iter().enumerate() {
                        window.paint_quad(
                            fill(
                                rect(
                                    px(38. + index as f32 * 16.),
                                    px(32.) + line_height,
                                    px(12.),
                                    px(4.),
                                ),
                                color,
                            )
                            .corner_radii(px(2.)),
                        );
                    }
                },
            )
            .size_full(),
        )
}

fn choose_theme(name: String, editing: &Entity<SettingsEditing>, cx: &mut App) {
    let theme = editing
        .read(cx)
        .theme_families
        .iter()
        .flat_map(|family| &family.variants)
        .find(|choice| choice.id == name)
        .map(|choice| choice.theme.clone())
        .ok_or_else(|| "theme is no longer available".to_string());

    let applied = select_theme(name, theme, cx);

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
    let families = &state.theme_families;
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
        .iter()
        .enumerate()
        .filter(|(_, family)| {
            filter.is_empty()
                || family.name.to_lowercase().contains(&filter)
                || family.variants.iter().any(|choice| {
                    choice.id.to_lowercase().contains(&filter)
                        || choice.theme.name.to_lowercase().contains(&filter)
                })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    let catalog = state.theme_families.clone();
    let measure = editing.downgrade();
    let row_height = THEME_CARD_HEIGHT + SPACE_3;
    let rows = families.len().div_ceil(usize::from(columns));
    let grid_height = (row_height * rows as f32 - SPACE_3).max(px(0.));

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
                .id("theme-grid")
                .w_full()
                .min_w_0()
                .relative()
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
                .child(
                    canvas(
                        move |bounds, window, cx| {
                            let visible = bounds.intersect(&window.content_mask().bounds);

                            let mut cards = Vec::new();

                            if visible.size.height <= px(0.) || visible.size.width <= px(0.) {
                                return cards;
                            }

                            let first_row = ((visible.top() - bounds.top()) / row_height)
                                .floor()
                                .max(0.) as usize;

                            let end_row = ((visible.bottom() - bounds.top()) / row_height)
                                .ceil()
                                .max(0.) as usize;

                            let width = ((bounds.size.width + SPACE_3) / f32::from(columns)
                                - SPACE_3)
                                .max(px(0.));

                            // The outer settings list owns scrolling. Reserve the entire
                            // grid height, but only lay out cards intersecting its clip.
                            for row in first_row..end_row.min(rows) {
                                for column in 0..usize::from(columns) {
                                    let Some(&index) =
                                        families.get(row * usize::from(columns) + column)
                                    else {
                                        break;
                                    };

                                    let mut card = div()
                                        .debug_selector(move || format!("theme-card-{index}"))
                                        .w(width)
                                        .h(THEME_CARD_HEIGHT)
                                        .child(theme_card(
                                            index,
                                            &catalog[index],
                                            mode,
                                            &selected,
                                            &editing,
                                            cx,
                                        ))
                                        .into_any_element();

                                    card.layout_as_root(
                                        size(width, THEME_CARD_HEIGHT).into(),
                                        window,
                                        cx,
                                    );

                                    card.prepaint_at(
                                        bounds.origin
                                            + point(
                                                (width + SPACE_3) * column as f32,
                                                row_height * row as f32,
                                            ),
                                        window,
                                        cx,
                                    );

                                    cards.push(card);
                                }
                            }

                            cards
                        },
                        |_, cards, window, cx| {
                            for mut card in cards {
                                card.paint(window, cx);
                            }
                        },
                    )
                    .w_full()
                    .h(grid_height),
                ),
        )
}

fn theme_card(
    index: usize,
    family: &ThemeFamily,
    mode: AppearanceTheme,
    selected: &str,
    editing: &Entity<SettingsEditing>,
    cx: &App,
) -> Button {
    let choice = family.variant(mode);
    let active = family.contains(selected);
    let id = choice.id.clone();
    let target = editing.clone();

    let name = if family.name.is_empty() {
        id.clone()
    } else {
        family.name.clone()
    };

    let border = cx.theme().border;
    let accent = cx.theme().primary;
    let hover = cx.theme().secondary;

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
        .bg(cx.theme().popover)
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
                    this.child(Icon::new(IconName::Check).size(px(14.)).text_color(accent))
                }),
        )
}
