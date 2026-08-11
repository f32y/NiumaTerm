use gpui_component::Disableable as _;

use crate::ui::background_tasks::{self, PANEL_TITLE};
use crate::ui::shell::*;

impl Shell {
    fn bind_actions(element: Div, cx: &mut Context<Self>) -> Div {
        element
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_new_workspace))
            .on_action(cx.listener(Self::on_next_workspace))
            .on_action(cx.listener(Self::on_prev_workspace))
            .on_action(cx.listener(Self::on_new_window))
            .on_action(cx.listener(Self::on_split_up))
            .on_action(cx.listener(Self::on_split_down))
            .on_action(cx.listener(Self::on_split_left))
            .on_action(cx.listener(Self::on_split_right))
            .on_action(cx.listener(Self::on_resize_pane_up))
            .on_action(cx.listener(Self::on_resize_pane_down))
            .on_action(cx.listener(Self::on_resize_pane_left))
            .on_action(cx.listener(Self::on_resize_pane_right))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_git_sidebar))
            .on_action(cx.listener(Self::on_toggle_background_tasks))
            .on_action(cx.listener(Self::on_show_settings))
            .on_action(cx.listener(Self::on_new_remote_tab))
            .on_action(cx.listener(Self::on_new_agent_tab))
    }

    fn render_title_bar(
        &mut self,
        tab_bar: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Interactive chrome lives in the titlebar but is wrapped in
        // `occlude()`: that blocks the drag hitbox beneath it, so Windows
        // treats these regions as client (clickable) while the empty titlebar
        // space stays draggable. The wrappers must size to their content (no
        // `flex_1`), or they'd cover the whole bar and leave nothing to drag.
        // Add future titlebar buttons the same way.
        TitleBar::new()
            // The default X calls `remove_window()` directly (no
            // WM_CLOSE), skipping `on_window_should_close` — so the
            // shared close confirmation is handled here too.
            .on_close_window(cx.listener(|this, _, window, cx| {
                if this.confirm_window_close(window, cx) {
                    window.remove_window();
                }
            }))
            .child(
                h_flex()
                    .w(px(self.sidebar.width - floating_surface::SIDE_INSET))
                    .flex_none()
                    .child(
                        div().occlude().child(
                            Button::new("toggle-sidebar")
                                .ghost()
                                .icon(SideBarIcon)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.on_toggle_sidebar(&ToggleSidebar, window, cx)
                                })),
                        ),
                    )
                    .child(
                        div().occlude().child(
                            Button::new("settings")
                                .ghost()
                                .icon(IconName::Settings)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.on_show_settings(&ShowSettings, window, cx)
                                })),
                        ),
                    )
                    .children(
                        cx.global::<AppSettings>()
                            .show_daily_token_usage
                            .then(|| div().occlude().child(self.token_usage.clone())),
                    ),
            )
            // The container keeps the title-bar drag area. Tabs and the
            // new-tab button block only their own bounds.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(tab_bar),
            )
            .child(
                h_flex()
                    .child(div().occlude().child(self.git_status.clone()))
                    .child(
                        div()
                            .occlude()
                            .child(self.render_background_tasks_button(cx)),
                    )
                    .child(
                        div().occlude().child(
                            Button::new("toggle-git-sidebar")
                                .ghost()
                                .icon(GitIcon)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.on_toggle_git_sidebar(&ToggleGitSidebar, window, cx)
                                })),
                        ),
                    ),
            )
    }

    /// Upper-right `Background Tasks` control. It carries the active child
    /// count and an unseen-activity dot for the current parent session, and is
    /// disabled when the active pane has no supported provider session.
    fn render_background_tasks_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self
            .active_agent()
            .and_then(|pane| pane.read(cx).background_tasks().cloned());
        let parent = self.active_task_parent(cx);
        let seen = parent
            .as_ref()
            .and_then(|parent| self.seen_task_activity.get(parent))
            .copied();
        let count = background_tasks::title_bar_label(snapshot.as_ref());
        // The indicator is suppressed while the view is open, because opening
        // it is what marks the current activity as seen.
        let unseen = !self
            .right_panel
            .read(cx)
            .shows(RightPanelKind::BackgroundTasks)
            && background_tasks::has_unseen_activity(snapshot.as_ref(), seen);
        let aria_label = match (&count, unseen) {
            (Some(count), true) => format!("{PANEL_TITLE}: {count} active, new activity"),
            (Some(count), false) => format!("{PANEL_TITLE}: {count} active"),
            (None, true) => format!("{PANEL_TITLE}: new activity"),
            (None, false) => PANEL_TITLE.to_string(),
        };

        div()
            .relative()
            .child(
                Button::new("toggle-background-tasks")
                    .ghost()
                    .icon(IconName::Bot)
                    .when_some(count, Button::label)
                    .aria_label(aria_label)
                    .tooltip(PANEL_TITLE)
                    .disabled(parent.is_none())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_toggle_background_tasks(&ToggleBackgroundTasks, window, cx)
                    })),
            )
            .when(unseen, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(4.0))
                        .right(px(4.0))
                        .size(px(6.0))
                        .rounded_full()
                        .bg(cx.theme().primary),
                )
            })
    }
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Titlebar sidebar-toggle icon, backed by the project's `assets/icons/
/// side-bar.svg` (served by `crate::assets::AppAssets`).
struct SideBarIcon;

impl IconNamed for SideBarIcon {
    fn path(self) -> SharedString {
        "icons/side-bar.svg".into()
    }
}

struct GitIcon;

impl IconNamed for GitIcon {
    fn path(self) -> SharedString {
        "icons/git.svg".into()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Safety net for any activation path that reaches a render without
        // passing `focus_active`: the visible tab must be live before anything
        // below reads the active pane.
        self.ensure_active_tab_live(window, cx);

        self.window_active = Self::exact_window_active(window);

        self.acknowledge_visible(window, false, cx);

        window.set_window_title(&self.active_tab_title());

        if self.needs_focus {
            self.needs_focus = false;
            self.focus_active(window, cx);
        }

        self.process_native_notifications(cx);

        // Any workspace/tab switch re-renders the shell, so this render-time
        // compare-and-set catches every switch path.
        self.sync_git_target(cx);
        self.sync_task_panel_target(cx);

        // The sidebar is always mounted so it can animate its width open/closed.
        let summaries = self.projected_workspace_summaries(cx);

        let sidebar = self.sidebar.render(
            summaries,
            self.workspace_rename.as_ref(),
            self.agent_usage.clone(),
            cx,
        );

        // Re-render the shell whenever the wrapping Root changes (dialog
        // open/close), since the shell draws the dialog layer.
        if !self.root_observed {
            if let Some(Some(root)) = window.root::<Root>() {
                cx.observe(&root, |_, _, cx| cx.notify()).detach();
                self.root_observed = true;
            }
        }

        // Root stores opened dialogs but does not draw them; the app renders the
        // dialog overlay itself.
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let update_notification_layer = self.render_update_notification_layer(cx);

        // Scroll the newly active tab into view on any switch path.
        let active_id = self.workspaces.active_tabs().active_id();
        let active_index = self.workspaces.active_tabs().active_index();

        self.tab_strip.reveal_active(active_id, active_index, cx);

        let (unread_tabs, busy_agent_tabs) = self.tab_agent_indicators(cx);

        let tab_bar = self.tab_strip.render(
            self.workspaces.active_tabs(),
            &unread_tabs,
            &busy_agent_tabs,
            self.tab_rename.as_ref(),
            cx,
        );

        self.apply_pending_ratios(cx);

        let pane_tree = self.render_active_tree(cx);

        let background_image = cx
            .global::<AppSettings>()
            .background_image
            .clone()
            .map(|path| {
                img(path::PathBuf::from(path))
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .opacity(ui::background_image_layer_opacity(cx))
            });

        let shell = div()
            .size_full()
            .relative()
            .overflow_hidden()
            // The window surface itself is never painted (gpui leaves it
            // white/transparent), and the chrome now has see-through regions —
            // the tab strip and the gutters around the terminal cards — so the
            // shell paints the chrome background across the whole window.
            // `apply_window_translucency` dims this color with the rest of the
            // chrome when window transparency is on.
            .bg(cx.theme().background)
            .flex()
            .flex_col()
            // All chrome inherits the configured UI font; terminal panes override it.
            .font_family(cx.global::<AppSettings>().ui_font_family.clone())
            .key_context("Shell");

        Self::bind_actions(shell, cx)
            .children(background_image)
            .child(self.render_title_bar(tab_bar, cx))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .child(sidebar)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .relative()
                            .overflow_hidden()
                            .px(px(floating_surface::SIDE_INSET))
                            .pt(px(floating_surface::TOP_INSET))
                            .pb(px(floating_surface::BOTTOM_INSET))
                            .child(
                                floating_surface::card(cx)
                                    .id("main-floating-surface")
                                    .min_w_0()
                                    .relative()
                                    .child(pane_tree)
                                    // Notifications are anchored to the pane
                                    // viewport inside the clipped card.
                                    .children(notification_layer),
                            ),
                    )
                    .child(self.right_panel.clone()),
            )
            .children(update_notification_layer)
            .children(dialog_layer)
    }
}
