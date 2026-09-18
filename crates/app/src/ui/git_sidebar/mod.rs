mod diff_view;
mod files;
mod tree;

#[cfg(test)]
mod tests;

use std::path::Path;

use app::design::REVIEW_FOOTER_HEIGHT;
use app::utils::on_runtime;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, KeyDownEvent, Point, Render,
    ScrollStrategy, UniformListScrollHandle, Window, div, px, relative,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Disableable, ElementExt, IconName, h_flex, v_flex};
use rust_i18n::t;

use crate::ui::composition::{GitColors, toolbar_button};
use crate::ui::git_sidebar::diff_view::DiffView;
use crate::ui::git_sidebar::files::ChangedFilesPanel;
use crate::ui::git_status::{GitStatusModel, fetch_file_diff};
use crate::ui::modern_dropdown;
use crate::ui::shell::{QuoteGitLine, ReturnFromGit};

enum ChangeDirection {
    Previous,
    Next,
}

/// One workspace's review state survives switches to conversations and other workspaces.
pub(crate) struct GitSidebar {
    model: Entity<GitStatusModel>,
    cwd: String,
    selected: Option<String>,
    diff: DiffView,
    diff_seq: u64,
    seen_snapshot_seq: u64,
    files: ChangedFilesPanel,
    diff_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    selected_line: Option<usize>,
    loading: bool,
    visible: bool,
    can_quote: bool,
    files_open: bool,
    wrap: bool,
    diff_width: f32,
}

impl GitSidebar {
    pub(crate) fn new(cwd: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = cx.new(GitStatusModel::for_tab);

        model.update(cx, |model, cx| model.set_target_cwd(Some(cwd.clone()), cx));

        let files = ChangedFilesPanel::new(window, cx);

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
            cwd,
            selected: None,
            diff: DiffView::default(),
            diff_seq: 0,
            seen_snapshot_seq: 0,
            files,
            diff_scroll: UniformListScrollHandle::default(),
            focus: cx.focus_handle(),
            selected_line: None,
            loading: false,
            visible: false,
            can_quote: false,
            files_open: true,
            wrap: false,
            diff_width: 700.0,
        }
    }

    pub(crate) fn cwd(&self) -> &str {
        &self.cwd
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    pub(crate) fn set_quote_available(&mut self, available: bool) {
        self.can_quote = available;
    }

    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }

        self.visible = visible;

        self.model.update(cx, |model, cx| {
            model.sidebar_open = visible;

            if visible {
                model.refresh(cx);
            }
        });
    }

    fn update_tree(&mut self, cx: &Context<Self>) {
        let model = self.model.read(cx);

        let files = model
            .snapshot
            .as_ref()
            .map_or(&[][..], |snapshot| &snapshot.files);

        self.files.rebuild(files, cx);
    }

    fn on_snapshot_changed(&mut self, cx: &mut Context<Self>) {
        self.update_tree(cx);

        let files = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .map(|snapshot| &snapshot.files);

        let next = self
            .selected
            .as_ref()
            .filter(|path| files.is_some_and(|files| files.iter().any(|file| &file.path == *path)))
            .cloned()
            .or_else(|| {
                files
                    .and_then(|files| files.first())
                    .map(|file| file.path.clone())
            });

        if next != self.selected {
            self.selected = next;
            self.diff = DiffView::default();
            self.selected_line = None;
            self.diff_seq += 1;

            self.reset_diff_scroll();
        }

        self.fetch_diff(cx);
    }

    fn reset_diff_scroll(&self) {
        self.diff_scroll
            .0
            .borrow()
            .base_handle
            .set_offset(Point::default());

        self.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn select_adjacent_file(&mut self, direction: ChangeDirection, cx: &mut Context<Self>) {
        if let Some(path) = self
            .files
            .adjacent_file(self.selected.as_deref(), direction)
        {
            self.select(path, cx);
        }
    }

    fn select(&mut self, path: String, cx: &mut Context<Self>) {
        self.files_open = true;

        if self.selected.as_deref() == Some(&path) {
            return;
        }

        self.selected = Some(path);
        self.diff = DiffView::default();
        self.selected_line = None;

        self.reset_diff_scroll();
        self.fetch_diff(cx);

        cx.notify();
    }

    fn fetch_diff(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(snapshot)) =
            (self.selected.clone(), self.model.read(cx).snapshot.as_ref())
        else {
            self.loading = false;

            return;
        };

        let root = snapshot.repo_root.clone();

        let untracked = snapshot
            .files
            .iter()
            .any(|file| file.path == path && file.status == "??");

        self.diff_seq += 1;

        let seq = self.diff_seq;

        self.loading = self.diff.is_empty();

        // Git runs on the shared runtime; preparing the diff is CPU work that
        // stays on the background executor.
        let fetch = cx.background_executor().spawn(async move {
            let diff_path = path.clone();

            let lines =
                on_runtime(async move { fetch_file_diff(&root, &diff_path, untracked).await })
                    .await;

            DiffView::prepare(lines, &path)
        });

        cx.spawn(async move |this, cx| {
            let prepared = fetch.await;

            this.update(cx, |this, cx| {
                if this.diff_seq == seq {
                    this.diff.update(prepared);

                    this.selected_line =
                        this.selected_line.filter(|index| *index < this.diff.len());

                    this.loading = false;

                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn selected_reference(&self, cx: &App) -> Option<String> {
        let root = &self.model.read(cx).snapshot.as_ref()?.repo_root;
        let path = self.selected.as_ref()?;
        let line = self.diff.line(self.selected_line?)?;
        let number = line.new_line.or(line.old_line)?;

        let side = if line.new_line.is_some() {
            "new"
        } else {
            "old"
        };

        Some(format!(
            "{}:{number} ({side})\n    {}",
            Path::new(root).join(path).display(),
            line.text
        ))
    }

    fn jump_change(&mut self, direction: ChangeDirection, cx: &mut Context<Self>) {
        let current = self.diff.current_change(&self.diff_scroll);

        let index = match direction {
            ChangeDirection::Next => (current + 1).min(self.diff.change_count().saturating_sub(1)),
            ChangeDirection::Previous => current.saturating_sub(1),
        };

        self.diff.jump_change(index, &self.diff_scroll);

        cx.notify();
    }
}

impl GitSidebar {
    fn render_review(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.diff.set_wrap(
            self.wrap
                .then(|| ((self.diff_width - 110.0) / 8.0).max(10.0) as usize),
        );

        let current = self.diff.current_change(&self.diff_scroll);
        let count = self.diff.change_count();
        let path = self.selected.clone().unwrap_or_default();
        let menu_view = cx.entity();

        let options = modern_dropdown(
            toolbar_button("git-display-options")
                .icon(IconName::Ellipsis)
                .tooltip(t!("git-display-options"))
                .accessibility_label(t!("git-display-options")),
            move |menu, _, _| {
                let wrap_view = menu_view.clone();
                let copy_view = menu_view.clone();

                menu.item(t!("git-wrap-lines"), move |_, cx| {
                    wrap_view.update(cx, |this, cx| {
                        this.wrap = !this.wrap;

                        cx.notify();
                    });
                })
                .item(t!("git-copy-path"), move |_, cx| {
                    let path = copy_view.read(cx).selected.clone();

                    if let Some(path) = path {
                        cx.write_to_clipboard(ClipboardItem::new_string(path));
                    }
                })
            },
        );

        let toolbar = h_flex()
            .h(px(42.0))
            .flex_none()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                toolbar_button("git-toggle-files")
                    .icon(IconName::PanelLeft)
                    .tooltip(t!("git-toggle-files"))
                    .accessibility_label(t!("git-toggle-files"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.files_open = !this.files_open;

                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("git-selected-path")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .child(path.clone())
                    .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx)),
            )
            .child(
                toolbar_button("git-previous-change")
                    .icon(IconName::ArrowUp)
                    .tooltip(t!("git-previous-change"))
                    .accessibility_label(t!("git-previous-change"))
                    .disabled(count == 0 || current == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.jump_change(ChangeDirection::Previous, cx)
                    })),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} / {count}",
                        if count == 0 { 0 } else { current + 1 }
                    )),
            )
            .child(
                toolbar_button("git-next-change")
                    .icon(IconName::ArrowDown)
                    .tooltip(t!("git-next-change"))
                    .accessibility_label(t!("git-next-change"))
                    .disabled(count == 0 || current + 1 >= count)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.jump_change(ChangeDirection::Next, cx)),
                    ),
            )
            .child(
                toolbar_button("git-refresh")
                    .icon(IconName::Redo)
                    .tooltip(t!("git-refresh"))
                    .accessibility_label(t!("git-refresh"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model.update(cx, |model, cx| model.refresh(cx))
                    })),
            )
            .child(options);

        let body = if self.loading {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!("git-loading-diff"))
                .into_any_element()
        } else if self.selected.is_none() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!("sidebar-git-select-file"))
                .into_any_element()
        } else {
            self.diff
                .render(&self.diff_scroll, Some(cx.entity()), self.selected_line, cx)
        };

        let colors = GitColors::new(cx);

        let rail = div()
            .w(px(9.0))
            .h_full()
            .relative()
            .flex_none()
            .bg(cx.theme().muted)
            .children(self.diff.change_positions().into_iter().map(
                |(index, fraction, removed)| {
                    div()
                        .id(("git-change-position", index))
                        .absolute()
                        .left(px(2.0))
                        .right(px(2.0))
                        .top(relative(fraction))
                        .h(px(6.0))
                        .bg(if removed {
                            colors.removed
                        } else {
                            colors.added
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.diff.jump_change(index, &this.diff_scroll);

                            cx.notify();
                        }))
                },
            ));

        let totals = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .files
                    .iter()
                    .find(|file| Some(&file.path) == self.selected.as_ref())
            })
            .map(|file| (file.added, file.removed));

        let measure = cx.weak_entity();

        v_flex()
            .h_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .child(toolbar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .on_prepaint(move |bounds, _, cx| {
                        let width = bounds.size.width.as_f32();

                        let _ = measure.update(cx, |this, cx| {
                            if (this.diff_width - width).abs() >= 1.0 {
                                this.diff_width = width;

                                if this.wrap {
                                    cx.notify();
                                }
                            }
                        });
                    })
                    .child(body)
                    .child(rail),
            )
            .child(
                h_flex()
                    .h(REVIEW_FOOTER_HEIGHT)
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_size(px(11.0))
                    .text_color(cx.theme().muted_foreground)
                    .when_some(totals, |this, (added, removed)| {
                        this.child(div().text_color(colors.added).child(format!("+{added}")))
                            .child(
                                div()
                                    .text_color(colors.removed)
                                    .child(format!("−{removed}")),
                            )
                    })
                    .child(div().flex_1())
                    .when(self.selected_line.is_some(), |this| {
                        this.child(
                            toolbar_button("git-quote-line")
                                .label(t!("git-quote-line"))
                                .disabled(!self.can_quote)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(QuoteGitLine), cx)
                                }),
                        )
                    })
                    .when(self.selected_line.is_none(), |this| {
                        this.child(t!("git-line-reference-hint"))
                    }),
            )
            .into_any_element()
    }
}

impl Render for GitSidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let files = self.files_open.then(|| {
            let snapshot = self.model.read(cx).snapshot.as_ref();
            let entries = snapshot.map(|snapshot| snapshot.files.clone());
            let branch = snapshot.and_then(|snapshot| snapshot.branch.clone());

            self.files.render(
                entries.unwrap_or_default(),
                self.selected.clone(),
                branch.unwrap_or_default(),
                cx,
            )
        });

        let review = self.render_review(cx);

        h_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(cx.theme().sidebar)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.files.filter_focused(window, cx) {
                    return;
                }

                match event.keystroke.key.as_str() {
                    "escape" => window.dispatch_action(Box::new(ReturnFromGit), cx),
                    "up" => this.select_adjacent_file(ChangeDirection::Previous, cx),
                    "down" => this.select_adjacent_file(ChangeDirection::Next, cx),
                    "[" => this.jump_change(ChangeDirection::Previous, cx),
                    "]" => this.jump_change(ChangeDirection::Next, cx),
                    _ => return,
                }

                cx.stop_propagation();
            }))
            .children(files)
            .child(review)
    }
}
