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
            .flex_row()
            .items_center()
            .size_full()
            .child(self.view.render(&self.scroll, None, None, cx))
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

    assert_eq!(before.size.height, px(24.0));
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

#[test]
fn tree_collapses_folder_chains_without_losing_duplicate_file_paths() {
    use crate::ui::git_sidebar::tree;
    use crate::ui::git_status::FileEntry;
    use std::collections::HashSet;

    let files: Vec<_> = ["src/view/menu.rs", "src/model/menu.rs", "README.md"]
        .into_iter()
        .map(|path| FileEntry {
            path: path.into(),
            status: " M".into(),
            added: 2,
            removed: 1,
        })
        .collect();

    let rows = tree::rows(&files, &HashSet::new(), "");

    let paths: Vec<_> = rows
        .iter()
        .filter(|row| row.file.is_some())
        .map(|row| row.path.as_str())
        .collect();

    assert_eq!(
        paths,
        ["src/model/menu.rs", "src/view/menu.rs", "README.md"]
    );

    let collapsed = HashSet::from(["src/model".to_string()]);
    let rows = tree::rows(&files, &collapsed, "");

    assert!(!rows.iter().any(|row| row.path == "src/model/menu.rs"));

    let filtered = tree::rows(&files, &collapsed, "MODEL/MENU");

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].file, Some(1));
    assert_eq!(filtered[0].label, "src/model/menu.rs");

    let compact = tree::rows(&files[..1], &HashSet::new(), "");

    assert_eq!(compact[0].label, "src/view");
    assert_eq!(compact[1].depth, 1);
}

#[test]
fn change_navigation_remembers_a_target_even_when_every_line_fits() {
    let lines = parse_diff("@@ -1,3 +1,3 @@\n-old\n+new\n context\n-before\n+after\n");

    let mut view = DiffView::new(lines.clone());

    let scroll = UniformListScrollHandle::default();

    assert_eq!(view.change_count(), 2);

    view.jump_change(1, &scroll);

    assert_eq!(view.current_change(&scroll), 1);

    view.update(DiffView::prepare(lines, "example.rs"));

    assert_eq!(view.current_change(&scroll), 1);

    view.jump_change(0, &scroll);

    assert_eq!(view.current_change(&scroll), 0);
}

#[gpui::test]
fn wrapping_keeps_unicode_source_and_removes_long_line_overflow(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.set_global(AppSettings::default());
    cx.update(install_terminal_settings);

    let source = "let label = \"路径🙂\"; ".repeat(20);

    let mut view = DiffView::new(parse_diff(&format!("@@ -0,0 +1 @@\n+{source}\n")));

    view.set_wrap(Some(24));

    assert_eq!(view.line(1).unwrap().text.as_ref(), source);
    assert_eq!(view.line(1).unwrap().new_line, Some(1));

    let scroll = UniformListScrollHandle::default();

    let handle = cx.add_window({
        let scroll = scroll.clone();

        move |_, _| DiffProbe { view, scroll }
    });

    let mut cx = VisualTestContext::from_window(handle.into(), cx);

    cx.simulate_resize(size(px(400.0), px(200.0)));
    cx.run_until_parked();
    cx.refresh().unwrap();

    let base = scroll.0.borrow().base_handle.clone();

    assert_eq!(base.max_offset().x, px(0.0));
    assert!(base.max_offset().y > px(200.0));
}
