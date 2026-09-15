use std::collections::HashSet;

use app::design::{
    REVIEW_FILES_MAX_WIDTH, REVIEW_FILES_MIN_WIDTH, REVIEW_FILES_WIDTH, REVIEW_FOOTER_HEIGHT,
    SURFACE_RADIUS,
};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DragMoveEvent, Entity, Focusable, Role, ScrollStrategy,
    UniformListScrollHandle, Window, div, px, relative, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use rust_i18n::t;

use crate::ui::composition::{GitColors, toolbar_button};
use crate::ui::git_sidebar::tree::TreeRow;
use crate::ui::git_sidebar::{ChangeDirection, GitSidebar, tree};
use crate::ui::git_status::FileEntry;
use crate::ui::sidebar_resize::{ResizeDrag, resize_handle};

const FILE_RESIZE: &str = "git-files-resize";

/// The changed-file column of the review: the file tree, its filter, the
/// directories folded in it, and the column's width. Selecting a file is the
/// review's to act on; everything else the column keeps to itself.
pub(super) struct ChangedFilesPanel {
    filter: Entity<InputState>,
    filter_open: bool,
    collapsed: HashSet<String>,
    rows: Vec<TreeRow>,
    scroll: UniformListScrollHandle,
    width: f32,
}

impl ChangedFilesPanel {
    /// A panel whose filter rebuilds the tree from the review's current
    /// snapshot as the query changes.
    pub(super) fn new(window: &mut Window, cx: &mut Context<GitSidebar>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx));

        cx.subscribe(&filter, |this: &mut GitSidebar, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.update_tree(cx);

                cx.notify();
            }
        })
        .detach();

        Self {
            filter,
            filter_open: false,
            collapsed: HashSet::new(),
            rows: Vec::new(),
            scroll: UniformListScrollHandle::default(),
            width: REVIEW_FILES_WIDTH.as_f32(),
        }
    }

    /// Lay the tree out again over `files`, keeping folded directories folded
    /// and applying the current filter.
    pub(super) fn rebuild(&mut self, files: &[FileEntry], cx: &App) {
        self.rows = tree::rows(
            files,
            &self.collapsed,
            self.filter.read(cx).value().as_ref(),
        );
    }

    pub(super) fn filter_focused(&self, window: &Window, cx: &App) -> bool {
        self.filter.read(cx).focus_handle(cx).is_focused(window)
    }

    /// The file next to `selected` in tree order, scrolled into view. With
    /// nothing selected the first file is next in either direction.
    pub(super) fn adjacent_file(
        &self,
        selected: Option<&str>,
        direction: ChangeDirection,
    ) -> Option<String> {
        let files: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.file.is_some())
            .collect();

        if files.is_empty() {
            return None;
        }

        let current = files
            .iter()
            .position(|(_, row)| Some(row.path.as_str()) == selected);

        let next = match (current, direction) {
            (Some(index), ChangeDirection::Next) => (index + 1).min(files.len() - 1),
            (Some(index), ChangeDirection::Previous) => index.saturating_sub(1),
            (None, _) => 0,
        };

        let (row, file) = files[next];

        self.scroll.scroll_to_item(row, ScrollStrategy::Center);

        Some(file.path.clone())
    }

    /// The column over `files`, highlighting `selected`, with `branch` in its
    /// footer.
    pub(super) fn render(
        &self,
        files: Vec<FileEntry>,
        selected: Option<String>,
        branch: String,
        cx: &mut Context<GitSidebar>,
    ) -> AnyElement {
        let rows = self.rows.clone();
        let count = files.len();
        let view = cx.entity();

        let list = uniform_list("git-files", rows.len(), move |range, _, cx| {
            range
                .map(|index| {
                    let row = &rows[index];
                    let path = row.path.clone();
                    let owner = view.clone();

                    let active =
                        row.file.is_some() && selected.as_deref() == Some(row.path.as_str());

                    let colors = GitColors::new(cx);

                    let status = row
                        .file
                        .and_then(|index| files.get(index))
                        .map(|file| file.status.trim());

                    h_flex()
                        .id(("git-file", index))
                        .role(Role::Button)
                        .h(px(28.0))
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .pl(px(8.0 + row.depth as f32 * 12.0))
                        .pr_2()
                        .gap_1()
                        .items_center()
                        .rounded(SURFACE_RADIUS)
                        .text_size(px(12.0))
                        .text_color(cx.theme().foreground)
                        .cursor_pointer()
                        .aria_label(row.path.clone())
                        .tooltip({
                            let path = path.clone();

                            move |window, cx| Tooltip::new(path.clone()).build(window, cx)
                        })
                        .when(active, |this| this.bg(cx.theme().list_active))
                        .hover(|this| this.bg(cx.theme().list_hover))
                        .child(if let Some(status) = status {
                            div()
                                .w(px(14.0))
                                .flex_none()
                                .text_size(px(10.0))
                                .text_color(if status == "??" || status == "A" {
                                    colors.added
                                } else if status == "D" {
                                    colors.removed
                                } else {
                                    cx.theme().warning
                                })
                                .child(if status == "??" {
                                    "+".to_string()
                                } else {
                                    status.to_string()
                                })
                                .into_any_element()
                        } else {
                            Icon::new(if row.expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(px(11.0))
                            .into_any_element()
                        })
                        .child(
                            Icon::new(if row.file.is_some() {
                                IconName::File
                            } else {
                                IconName::Folder
                            })
                            .size(px(13.0))
                            .text_color(cx.theme().muted_foreground),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(row.label.clone()))
                        .on_click({
                            let file = row.file;

                            move |_, window, cx| {
                                owner.update(cx, |this, cx| {
                                    if file.is_some() {
                                        this.select(path.clone(), cx);
                                        this.focus(window, cx);
                                    } else {
                                        if !this.files.collapsed.remove(&path) {
                                            this.files.collapsed.insert(path.clone());
                                        }

                                        this.update_tree(cx);

                                        cx.notify();
                                    }
                                });
                            }
                        })
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .size_full();

        v_flex()
            .w(px(self.width))
            .max_w(relative(0.45))
            .h_full()
            .flex_none()
            .min_h_0()
            .relative()
            .border_r_1()
            .border_color(cx.theme().border)
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeDrag>, _, cx| {
                    if event.drag(cx).is_from(FILE_RESIZE) {
                        this.files.width = (event.event.position.x - event.bounds.left())
                            .as_f32()
                            .clamp(
                                REVIEW_FILES_MIN_WIDTH.as_f32(),
                                REVIEW_FILES_MAX_WIDTH.as_f32(),
                            );

                        cx.notify();
                    }
                }),
            )
            .child(
                h_flex()
                    .h(px(42.0))
                    .px_3()
                    .gap_2()
                    .flex_none()
                    .text_size(px(12.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .child(t!("git-changed-files", count = count)),
                    )
                    .child(
                        toolbar_button("git-filter")
                            .icon(IconName::Search)
                            .tooltip(t!("git-filter-files"))
                            .accessibility_label(t!("git-filter-files"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.files.filter_open = !this.files.filter_open;

                                if this.files.filter_open {
                                    this.files
                                        .filter
                                        .update(cx, |input, cx| input.focus(window, cx));
                                } else {
                                    this.files
                                        .filter
                                        .update(cx, |input, cx| input.set_value("", window, cx));

                                    this.update_tree(cx);
                                }

                                cx.notify();
                            })),
                    ),
            )
            .when(self.filter_open, |this| {
                this.child(div().px_2().pb_2().child(Input::new(&self.filter).small()))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .px_1()
                    .child(list)
                    .when(self.rows.is_empty(), |this| {
                        this.child(
                            div()
                                .absolute()
                                .inset_0()
                                .p_3()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("sidebar-git-no-changes")),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(12.0))
                            .child(Scrollbar::vertical(&self.scroll)),
                    ),
            )
            .child(
                h_flex()
                    .h(REVIEW_FOOTER_HEIGHT)
                    .flex_none()
                    .px_3()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_size(px(11.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::GitBranch).size(px(12.0)))
                    .child(div().truncate().child(branch)),
            )
            .child(resize_handle(FILE_RESIZE, false, cx))
            .into_any_element()
    }
}
