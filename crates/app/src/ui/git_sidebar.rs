use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, ScrollStrategy, UniformListScrollHandle, Window, div, px,
    uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, IconName, Sizable as _, h_flex, v_flex};
use nmt_i18n::i18n;

use crate::terminal::metrics;
use crate::ui::git_status::{DiffLine, DiffLineKind, GitStatusModel, fetch_file_diff};

/// Git content for the shared right-side host. Open state, width, slide
/// animation, resizing, and the outer card belong to that host, so Git and
/// `Background Tasks` cannot disagree about the geometry they share.
pub(crate) struct GitSidebar {
    model: Entity<GitStatusModel>,
    selected: Option<String>,
    diff: Vec<DiffLine>,
    /// Guards a slow diff fetch from overwriting a newer selection's diff.
    diff_seq: u64,
    /// Last `snapshot_seq` reacted to, so `refreshing` flag flips don't
    /// re-fetch the diff.
    seen_snapshot_seq: u64,
    files_scroll: UniformListScrollHandle,
    diff_scroll: UniformListScrollHandle,
}

impl GitSidebar {
    pub(crate) fn new(model: Entity<GitStatusModel>, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |this: &mut Self, model, cx| {
            let seq = model.read(cx).snapshot_seq;
            if seq != this.seen_snapshot_seq {
                this.seen_snapshot_seq = seq;
                this.on_snapshot_changed(cx);
            }
            cx.notify();
        })
        .detach();

        Self {
            model,
            selected: None,
            diff: Vec::new(),
            diff_seq: 0,
            seen_snapshot_seq: 0,
            files_scroll: UniformListScrollHandle::default(),
            diff_scroll: UniformListScrollHandle::default(),
        }
    }

    fn on_snapshot_changed(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self.selected.clone() else {
            return;
        };

        let still_listed = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .is_some_and(|s| s.files.iter().any(|f| f.path == selected));

        if still_listed {
            self.fetch_diff(cx);
        } else {
            self.selected = None;
            self.diff.clear();
            self.diff_seq += 1;
        }
    }

    fn select(&mut self, path: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(&path) {
            self.selected = Some(path);

            self.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);

            self.fetch_diff(cx);

            cx.notify();
        }
    }

    fn fetch_diff(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(snapshot)) =
            (self.selected.clone(), self.model.read(cx).snapshot.as_ref())
        else {
            return;
        };

        let root = snapshot.repo_root.clone();

        let untracked = snapshot
            .files
            .iter()
            .any(|f| f.path == path && f.status == "??");

        self.diff_seq += 1;

        let seq = self.diff_seq;

        let fetch = cx
            .background_executor()
            .spawn(async move { fetch_file_diff(&root, &path, untracked) });

        cx.spawn(async move |this, cx| {
            let lines = fetch.await;

            this.update(cx, |this, cx| {
                if this.diff_seq == seq {
                    this.diff = lines;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_file_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let file_count = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .map_or(0, |s| s.files.len());

        if file_count == 0 {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(i18n("sidebar-git-no-changes"))
                .into_any_element();
        }

        let model = self.model.clone();
        let sidebar = cx.entity();
        let selected = self.selected.clone();

        div()
            .flex_1()
            .relative()
            .overflow_hidden()
            .child(
                uniform_list("git-files", file_count, move |range, _window, cx| {
                    let Some(snapshot) = model.read(cx).snapshot.as_ref() else {
                        return Vec::new();
                    };

                    range
                        .filter_map(|ix| snapshot.files.get(ix).cloned().map(|f| (ix, f)))
                        .map(|(ix, file)| {
                            let is_selected = selected.as_deref() == Some(file.path.as_str());
                            let sidebar = sidebar.clone();
                            let path = file.path.clone();
                            let theme = cx.theme();

                            h_flex()
                                .id(("git-file", ix))
                                // Full width pins the row to the list width so
                                // the path truncates instead of the row growing
                                // past the sidebar (and the window edge).
                                .w_full()
                                .overflow_hidden()
                                .h(px(24.0))
                                .px_2()
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .cursor_pointer()
                                .when(is_selected, |this| this.bg(theme.list_active))
                                .hover(|this| this.bg(theme.list_hover))
                                .child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .child(file.status.trim().to_string()),
                                )
                                .child(div().flex_1().truncate().child(file.path.clone()))
                                .child(
                                    div()
                                        .text_color(theme.green)
                                        .child(format!("+{}", file.added)),
                                )
                                .child(
                                    div()
                                        .text_color(theme.red)
                                        .child(format!("-{}", file.removed)),
                                )
                                .on_click(move |_, _, cx| {
                                    sidebar.update(cx, |this, cx| this.select(path.clone(), cx));
                                })
                        })
                        .collect()
                })
                .track_scroll(&self.files_scroll)
                .h_full(),
            )
            .child(scrollbar(&self.files_scroll))
            .into_any_element()
    }

    fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.selected.is_none() {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(i18n("sidebar-git-select-file"))
                .into_any_element();
        }

        let line_count = self.diff.len();
        let sidebar = cx.entity();

        div()
            .flex_1()
            .relative()
            .overflow_hidden()
            .font_family(metrics::font_family(cx))
            .text_size(px(12.0))
            .child(
                uniform_list("git-diff", line_count, move |range, _window, cx| {
                    let theme = cx.theme();
                    let sidebar = sidebar.read(cx);

                    range
                        .filter_map(|ix| sidebar.diff.get(ix))
                        .map(|line| {
                            let color = match line.kind {
                                DiffLineKind::Added => theme.green,
                                DiffLineKind::Removed => theme.red,
                                DiffLineKind::Hunk => theme.cyan,
                                DiffLineKind::FileHeader | DiffLineKind::Truncated => {
                                    theme.muted_foreground
                                }
                                DiffLineKind::Context => theme.foreground,
                            };

                            div()
                                // Full width + truncate clips long diff lines
                                // at the sidebar edge (ellipsis marks the cut)
                                // instead of overflowing the window.
                                .w_full()
                                .h(px(18.0))
                                .px_2()
                                .truncate()
                                .text_color(color)
                                .child(line.text.clone())
                        })
                        .collect()
                })
                .track_scroll(&self.diff_scroll)
                .h_full(),
            )
            .child(scrollbar(&self.diff_scroll))
            .into_any_element()
    }
}

/// Right-edge overlay scrollbar for a `uniform_list`. The `Scrollbar` element
/// marks itself `position: absolute` but sets no inset, so on its own it lands
/// at the static flex position — after the list, outside the clip. This wrapper
/// pins it to the right edge explicitly.
fn scrollbar(handle: &UniformListScrollHandle) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(16.0))
        .child(Scrollbar::vertical(handle))
}

impl Render for GitSidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let in_repo = model.read(cx).snapshot.is_some();

        let header = h_flex()
            .px_2()
            .py_1()
            .justify_between()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .child(div().text_sm().child(i18n("sidebar-git-title")))
            .child(
                Button::new("git-refresh")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Redo)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model.update(cx, |model, cx| model.refresh(cx));
                    })),
            );

        let body: AnyElement = if in_repo {
            v_flex()
                .flex_1()
                .overflow_hidden()
                .child(self.render_file_list(cx))
                .child(div().border_t_1().border_color(cx.theme().sidebar_border))
                .child(self.render_diff(cx))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(i18n("sidebar-git-not-repo"))
                .into_any_element()
        };

        v_flex().size_full().child(header).child(body)
    }
}
