use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, ListSizingBehavior, ScrollStrategy, Subscription,
    div, px, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;
use gpui_component::{
    ActiveTheme as _, Sizable as _, VirtualListScrollHandle, h_flex, v_virtual_list,
};
use nmt_agent::team::storage::history::{RoomSummary, recent_rooms};
use rust_i18n::t;

use crate::agent_tab::session::{directories_match, directory_label};
use crate::agent_tab::settings::UI_RADIUS;
use crate::agent_tab::team::TeamRuntime;
use crate::agent_tab::team::view::TeamPane;
use crate::agent_tab::transcript::relative_time;
use crate::agent_tab::view::composer_layout::{composer_panel, composer_panel_slot};
use crate::agent_tab::view::recent_sessions::RecentSessionsMode;

pub(super) struct TeamHistory {
    pub(super) directory: PathBuf,
    pub(super) mode: RecentSessionsMode,
    pub(super) selected: usize,
    pub(super) pending: Option<(Entity<TeamRuntime>, Subscription)>,
    sessions: Vec<RoomSummary>,
    all_directories: bool,
    loading: bool,
    scroll: VirtualListScrollHandle,
}

impl TeamHistory {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            mode: RecentSessionsMode::Automatic,
            selected: 0,
            pending: None,
            sessions: Vec::new(),
            all_directories: false,
            loading: false,
            scroll: VirtualListScrollHandle::new(),
        }
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<TeamPane>) {
        if self.loading {
            return;
        }

        self.loading = true;

        let directory = self.directory.clone();

        let task = cx
            .background_executor()
            .spawn(async move { recent_rooms(&directory) });

        cx.spawn(async move |this, cx| {
            let result = task.await;

            let _ = this.update(cx, |this, cx| {
                this.history.loading = false;

                match result {
                    Ok(sessions) => this.history.sessions = sessions,
                    Err(error) => this.error = Some(error.to_string()),
                }

                this.history.selected = 0;

                cx.notify();
            });
        })
        .detach();

        cx.notify();
    }

    pub(super) fn rows(&self, pane: &TeamPane, cx: &App) -> Vec<RoomSummary> {
        let runtime = pane.runtime.read(cx);

        self.sessions
            .iter()
            .filter(|session| {
                session.id != runtime.id()
                    && (self.all_directories
                        || directories_match(
                            session.cwd.as_deref(),
                            runtime.room().workspace().primary(),
                        ))
            })
            .cloned()
            .collect()
    }

    pub(super) fn visible(&self, pane: &TeamPane, cx: &App) -> bool {
        let room = pane.runtime.read(cx).room();

        self.pending.is_none()
            && (self.mode == RecentSessionsMode::Open
                || self.mode.is_visible(
                    room.discussions().is_empty() && room.messages().is_empty(),
                    pane.input.read(cx).text().len() == 0,
                    self.rows(pane, cx).len(),
                ))
    }

    pub(super) fn move_selection(&mut self, count: usize, next: bool) {
        if count == 0 {
            return;
        }

        self.selected = if next {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };

        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
    }

    pub(super) fn toggle_scope(&mut self, cx: &mut Context<TeamPane>) {
        self.all_directories = !self.all_directories;
        self.selected = 0;

        self.refresh(cx);
    }

    pub(super) fn render(&self, rows: Vec<RoomSummary>, cx: &mut Context<TeamPane>) -> AnyElement {
        let count = rows.len();

        let body = if count == 0 {
            div()
                .px_2()
                .py_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!(if self.loading {
                    "team-history-loading"
                } else {
                    "team-history-empty"
                }))
                .into_any_element()
        } else {
            let list = v_virtual_list(
                cx.entity(),
                "team-history-list",
                Rc::new(vec![size(px(0.), px(32.)); count]),
                move |pane, range, _, cx| {
                    range
                        .map(|index| {
                            let row = &rows[index];
                            let id = row.id;

                            h_flex()
                                .id(("team-history-row", index))
                                .h(px(32.))
                                .w_full()
                                .px_2()
                                .gap_2()
                                .items_center()
                                .rounded(UI_RADIUS)
                                .cursor_pointer()
                                .when(pane.history.selected == index, |row| {
                                    row.bg(cx.theme().list_active)
                                })
                                .hover(|row| row.bg(cx.theme().list_hover))
                                .on_click(cx.listener(move |pane, _, window, cx| {
                                    pane.resume_room(id, window, cx)
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .child(row.title.clone()),
                                )
                                .when_some(
                                    row.cwd.clone().filter(|_| pane.history.all_directories),
                                    |view, cwd| {
                                        view.child(
                                            div()
                                                .max_w(px(160.))
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(directory_label(&cwd)),
                                        )
                                    },
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(relative_time(row.last_active)),
                                )
                                .into_any_element()
                        })
                        .collect()
                },
            )
            .track_scroll(&self.scroll)
            .with_sizing_behavior(ListSizingBehavior::Infer);

            div()
                .relative()
                .w_full()
                .h(px((count.min(10) * 32) as f32))
                .flex_none()
                .overflow_hidden()
                .px_2()
                .child(list)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.))
                        .child(Scrollbar::vertical(&self.scroll)),
                )
                .into_any_element()
        };

        composer_panel_slot(
            composer_panel(cx)
                .child(
                    h_flex()
                        .px_2()
                        .pt_2()
                        .pb_1()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("agent-history-recent-sessions")),
                        )
                        .child(
                            Button::new("team-history-scope")
                                .ghost()
                                .small()
                                .label(t!(if self.all_directories {
                                    "agent-history-all-directories"
                                } else {
                                    "agent-history-current-directory"
                                }))
                                .on_click(cx.listener(|pane, _, _, cx| {
                                    pane.history.toggle_scope(cx);
                                })),
                        ),
                )
                .child(body),
        )
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .into_any_element()
    }
}
