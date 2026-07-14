use std::rc::Rc;

use gpui::{AnyElement, App, IntoElement, ParentElement as _, StyleRefinement, Window, div};

use crate::checkbox::Checkbox;
use crate::setting::fields::{SettingFieldRender, get_value, set_value};
use crate::setting::{AnySettingField, RenderOptions};
use crate::switch::Switch;
use crate::{Disableable, Sizable, StyledExt};

pub(crate) struct BoolField {
    use_switch: bool,
}

impl BoolField {
    pub(crate) fn new(use_switch: bool) -> Self {
        Self { use_switch }
    }
}

impl SettingFieldRender for BoolField {
    fn render(
        &self,
        field: Rc<dyn AnySettingField>,
        options: &RenderOptions,
        style: &StyleRefinement,
        _: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let checked = get_value::<bool>(&field, cx);
        let set_value = set_value::<bool>(&field, cx);

        div()
            .refine_style(style)
            .child(if self.use_switch {
                Switch::new("check")
                    .checked(checked)
                    .disabled(options.disabled)
                    .with_size(options.size)
                    .on_click(move |checked: &bool, _, cx: &mut App| {
                        set_value(*checked, cx);
                    })
                    .into_any_element()
            } else {
                Checkbox::new("check")
                    .checked(checked)
                    .disabled(options.disabled)
                    .with_size(options.size)
                    .on_click(move |checked: &bool, _, cx: &mut App| {
                        set_value(*checked, cx);
                    })
                    .into_any_element()
            })
            .into_any_element()
    }
}
