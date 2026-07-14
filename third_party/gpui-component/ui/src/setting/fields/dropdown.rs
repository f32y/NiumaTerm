use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{Anchor, AnyElement, App, IntoElement, SharedString, StyleRefinement, Styled, Window};

use crate::button::Button;
use crate::menu::{DropdownMenu, PopupMenuItem};
use crate::setting::fields::{SettingFieldRender, get_value, set_value};
use crate::setting::{AnySettingField, RenderOptions};
use crate::{AxisExt, Disableable, Sizable, StyledExt};

pub(crate) struct DropdownField<T> {
    options: Vec<(SharedString, SharedString)>,
    scrollable: bool,
    _marker: std::marker::PhantomData<T>,
}

impl<T> DropdownField<T> {
    pub(crate) fn new(
        options: Option<&Vec<(SharedString, SharedString)>>,
        scrollable: bool,
    ) -> Self {
        Self {
            options: options.cloned().unwrap_or_default(),
            scrollable,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> SettingFieldRender for DropdownField<T>
where
    T: Into<SharedString> + From<SharedString> + Clone + 'static,
{
    fn render(
        &self,
        field: Rc<dyn AnySettingField>,
        options: &RenderOptions,
        style: &StyleRefinement,
        _: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let old_value = get_value::<T>(&field, cx);
        let set_value = set_value::<T>(&field, cx);
        let dropdown_options = self.options.clone();
        let scrollable = self.scrollable;

        let old_label = dropdown_options
            .iter()
            .find(|(value, _)| *value == old_value.clone().into())
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| old_value.clone().into());

        Button::new("btn")
            .when(options.layout.is_vertical(), |this| this.w_full())
            .label(old_label)
            .dropdown_caret(true)
            .outline()
            .disabled(options.disabled)
            .with_size(options.size)
            .refine_style(style)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let set_value = set_value.clone();
                let menu = dropdown_options.iter().fold(menu, |menu, (value, label)| {
                    let old_value: SharedString = old_value.clone().into();
                    let checked = &old_value == value;
                    menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(checked)
                            .on_click({
                                let value = value.clone();
                                let set_value = set_value.clone();
                                move |_, _, cx| {
                                    set_value(T::from(value.clone()), cx);
                                }
                            }),
                    )
                });
                menu.scrollable(scrollable)
            })
            .into_any_element()
    }
}
