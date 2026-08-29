use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, font,
};

use crate::modern_menu::{MenuView, Row, metrics, snapshot};
use crate::{ActiveTheme as _, Icon, IconName};

impl Render for MenuView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let dark = theme.is_dark();
        let enabled_color = theme.popover_foreground;
        let disabled_color = theme.muted_foreground;
        let hover_color =
            if dark { gpui::white() } else { gpui::black() }.opacity(metrics::hover_alpha(dark));
        let pressed_color =
            if dark { gpui::white() } else { gpui::black() }.opacity(metrics::pressed_alpha(dark));
        let separator_color = if dark { gpui::white() } else { gpui::black() }
            .opacity(metrics::separator_alpha(dark));
        let stroke_color = gpui::black().opacity(metrics::surface_stroke_alpha(dark));
        let tint_color = theme.tokens.popover.opacity(metrics::TINT_ALPHA);

        let selected = self.selected;
        // Snapshotted so the closures below can move what they need without
        // borrowing the view they are being built inside. Command buttons carry
        // an id of their own because their entry holds several of them.
        let mut next_button = 0;
        let rows: Vec<Row> = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                crate::modern_menu::Entry::Separator => Row::Separator,
                crate::modern_menu::Entry::Item(item) => Row::Item(index, snapshot(item)),
                crate::modern_menu::Entry::Submenu(submenu) => {
                    Row::Submenu(index, submenu.label.clone(), submenu.icon.clone())
                }
                crate::modern_menu::Entry::Commands(items) => Row::Commands(
                    items
                        .iter()
                        .map(|item| {
                            next_button += 1;
                            (next_button, snapshot(item))
                        })
                        .collect(),
                ),
            })
            .collect();

        div()
            .size_full()
            // The material itself comes from underneath this window; what is
            // painted here is the tint and the inner surface stroke over it.
            .bg(tint_color)
            .py(metrics::PRESENTER_PADDING_Y)
            .rounded(metrics::CORNER_RADIUS)
            .border(metrics::BORDER_WIDTH)
            .border_color(stroke_color)
            .font(
                self.font
                    .clone()
                    .unwrap_or_else(|| font(cx.theme().font_family.clone())),
            )
            .text_size(metrics::FONT_SIZE)
            .flex()
            .flex_col()
            .children(rows.into_iter().map(|row| {
                let (index, (label, disabled, activation, icon)) = match row {
                    Row::Separator => {
                        return div()
                            .h(metrics::SEPARATOR_HEIGHT)
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .h(metrics::SEPARATOR_THICKNESS)
                                    .w_full()
                                    .bg(separator_color),
                            )
                            .into_any_element();
                    }
                    Row::Commands(buttons) => {
                        return div()
                            .h(metrics::COMMAND_ROW_HEIGHT)
                            .px(metrics::ITEM_MARGIN_X)
                            .flex()
                            .items_center()
                            // Centred rather than left-aligned: the buttons keep
                            // a fixed width, so in a menu made wide by its labels
                            // a left-aligned row sits off to one side.
                            .justify_center()
                            .children(buttons.into_iter().map(
                                |(id, (label, disabled, activation, icon))| {
                                    let button = div()
                                        .id(("modern-menu-command", id))
                                        .w(metrics::COMMAND_BUTTON_WIDTH)
                                        .h_full()
                                        .rounded(metrics::ITEM_RADIUS)
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .justify_center()
                                        .gap(metrics::COMMAND_LABEL_GAP)
                                        .child(div().flex_none().size(metrics::ICON_SIZE).children(
                                            icon.map(|icon| icon.size(metrics::ICON_SIZE)),
                                        ))
                                        .child(
                                            div()
                                                .text_size(metrics::COMMAND_LABEL_SIZE)
                                                .child(label),
                                        );

                                    if disabled {
                                        return button.text_color(disabled_color);
                                    }

                                    button
                                        .text_color(enabled_color)
                                        .hover(|this| this.bg(hover_color))
                                        .active(|this| this.bg(pressed_color))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                this.choose(activation.clone(), cx);
                                            }),
                                        )
                                },
                            ))
                            .into_any_element();
                    }
                    Row::Submenu(index, label, icon) => {
                        // The row stays lit while its submenu is up, so the two
                        // surfaces read as one path rather than as a menu that
                        // appeared beside an unrelated row.
                        let lit = selected == Some(index) || self.open_child == Some(index);

                        return div()
                            .h(metrics::item_height(self.input))
                            .px(metrics::ITEM_MARGIN_X)
                            .py(metrics::ITEM_MARGIN_Y)
                            .child(
                                div()
                                    .id(("modern-menu-submenu", index))
                                    .h_full()
                                    .px(metrics::ITEM_PADDING_X)
                                    .rounded(metrics::ITEM_RADIUS)
                                    .flex()
                                    .items_center()
                                    .gap(metrics::ICON_GAP)
                                    .text_color(enabled_color)
                                    .when(lit, |this| this.bg(hover_color))
                                    .hover(|this| this.bg(hover_color))
                                    .child(
                                        div().flex_none().size(metrics::ICON_SIZE).children(
                                            icon.map(|icon| icon.size(metrics::ICON_SIZE)),
                                        ),
                                    )
                                    .child(label)
                                    .child(div().flex_1())
                                    .child(
                                        Icon::new(IconName::ChevronRight)
                                            .size(metrics::CHEVRON_SIZE),
                                    )
                                    // Pointing at the row is what opens it; a
                                    // press is accepted as well, for a pointer
                                    // that arrived by clicking rather than by
                                    // travelling across the menu.
                                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                        this.selected = Some(index);
                                        this.open_submenu(index, false, cx);
                                        cx.notify();
                                    }))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.open_submenu(index, false, cx);
                                            cx.notify();
                                        }),
                                    ),
                            )
                            .into_any_element();
                    }
                    Row::Item(index, snapshot) => (index, snapshot),
                };

                let item = div()
                    .id(("modern-menu-item", index))
                    .h_full()
                    .px(metrics::ITEM_PADDING_X)
                    .rounded(metrics::ITEM_RADIUS)
                    .flex()
                    .items_center()
                    .gap(metrics::ICON_GAP)
                    // Kept even when the item has no icon, so the labels of one
                    // menu line up with each other.
                    .child(
                        div()
                            .flex_none()
                            .size(metrics::ICON_SIZE)
                            .children(icon.map(|icon| icon.size(metrics::ICON_SIZE))),
                    )
                    .child(label);

                if disabled {
                    return div()
                        .h(metrics::item_height(self.input))
                        .px(metrics::ITEM_MARGIN_X)
                        .py(metrics::ITEM_MARGIN_Y)
                        .child(item.text_color(disabled_color))
                        .into_any_element();
                }

                let item = item
                    .text_color(enabled_color)
                    .when(selected == Some(index), |this| this.bg(hover_color))
                    .hover(|this| this.bg(hover_color))
                    .active(|this| this.bg(pressed_color))
                    // Moving the pointer takes the highlight back from the
                    // keyboard, so the two never show a selection at once.
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.selected != Some(index) {
                            this.selected = Some(index);
                            cx.notify();
                        }
                        // The submenu belongs to the row that opened it, which
                        // is no longer the row being pointed at.
                        this.close_submenu(cx);
                    }))
                    // Menus commit on release, so a press that started outside
                    // and ended on an item still chooses it.
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.choose(activation.clone(), cx);
                        }),
                    );

                div()
                    .h(metrics::item_height(self.input))
                    .px(metrics::ITEM_MARGIN_X)
                    .py(metrics::ITEM_MARGIN_Y)
                    .child(item)
                    .into_any_element()
            }))
    }
}
