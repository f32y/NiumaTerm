use gpui::prelude::*;
use gpui::{
    Context, TestAppContext, UniformListScrollHandle, VisualTestContext, Window, div, point, px,
    size,
};

use crate::ui::git_sidebar::diff_view::DiffView;
use crate::ui::git_status::parse_diff;
use crate::ui::{AppSettings, install_terminal_settings};

struct DiffProbe {
    view: DiffView,
    scroll: UniformListScrollHandle,
}

impl Render for DiffProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.view.render(&self.scroll, cx))
    }
}

#[gpui::test]
fn diff_long_code_scrolls_without_truncation(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.set_global(AppSettings::default());
    cx.update(install_terminal_settings);
    let scroll = UniformListScrollHandle::default();
    let handle = cx.add_window({
        let scroll = scroll.clone();
        move |_, _| DiffProbe {
            view: DiffView::new(parse_diff(&format!(
                "@@ -12,2 +12,2 @@\n context\n-old\n+{}\n",
                "long_code_".repeat(80)
            ))),
            scroll,
        }
    });
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    cx.simulate_resize(size(px(400.0), px(200.0)));
    cx.run_until_parked();
    cx.refresh().unwrap();
    let before = cx.debug_bounds("diff-row-3").unwrap();
    assert_eq!(before.size.height, px(20.0));
    assert_eq!(before.size.width, px(400.0));
    let base = scroll.0.borrow().base_handle.clone();
    assert!(base.max_offset().x > px(400.0));
    base.set_offset(point(px(-200.0), px(0.0)));
    cx.update(|window, cx| {
        window
            .root::<DiffProbe>()
            .flatten()
            .unwrap()
            .update(cx, |_, cx| cx.notify());
    });
    cx.refresh().unwrap();
    let after = cx.debug_bounds("diff-row-3").unwrap();
    assert_eq!(after.origin.x, before.origin.x - px(200.0));
    assert_eq!(after.size.width, before.size.width + px(200.0));
    assert_eq!(after.size.height, before.size.height);
}
