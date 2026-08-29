use gpui::KeyDownEvent;
use gpui_component::modern_menu::{ModernMenu, dispatch_modern_menu_key};
use nmt_app_agent::RecoveryIdentity;
use nmt_i18n::i18n;

use crate::ui::shell::*;
use crate::ui::tab_bar::new_tab_menu;
use crate::update::check_now;

/// Width the tab strip keeps once the title bar runs out of room: about one
/// truncated tab plus the new-tab button, so the strip stays visible and its
/// horizontal scroll stays reachable at the window's minimum width.
pub(super) const TAB_STRIP_MIN_WIDTH: f32 = 120.0;

/// The bar is taller than the Fluent standard strip because it carries
/// controls, a wordmark and a session heading rather than a title alone.
const TITLE_BAR_HEIGHT: f32 = 44.0;
/// A leading-zone control: square, and spaced tightly enough that the group
/// reads as one cluster rather than as separate buttons.
const TITLE_BAR_BUTTON: f32 = 26.0;
const TITLE_BAR_BUTTON_GAP: f32 = 4.0;
/// Separation between the control cluster and the wordmark it precedes.
const TITLE_BAR_WORDMARK_INSET: f32 = 6.0;
const TITLE_BAR_WORDMARK_TEXT: f32 = 13.0;
/// The session heading in the middle of the bar, and the branch chip beside
/// it. The chip is set smaller than the title because it qualifies the title
/// rather than competing with it.
const TITLE_BAR_HEADING_TEXT: f32 = 13.0;
const TITLE_BAR_HEADING_GAP: f32 = 10.0;
const TITLE_BAR_CHIP_TEXT: f32 = 12.0;
const TITLE_BAR_CHIP_RADIUS: f32 = 6.0;
const TITLE_BAR_CHIP_PADDING_X: f32 = 8.0;
const TITLE_BAR_CHIP_PADDING_Y: f32 = 2.0;
const TITLE_BAR_CHIP_ICON: f32 = 11.0;
/// The mark on the busy-session control. The control only appears while work
/// is in flight, and the dot is what says so before the arrow is read as
/// navigation rather than as decoration.
const TITLE_BAR_BADGE: f32 = 6.0;

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
        // Vertical tabs move the strip into the sidebar, which leaves the
        // middle of the bar free to name the session on screen instead.
        let vertical_tabs = cx.global::<AppSettings>().tab_bar_style == TabBarStyle::Vertical;

        // Interactive chrome lives in the titlebar but is wrapped in
        // `occlude()`: that blocks the drag hitbox beneath it, so Windows
        // treats these regions as client (clickable) while the empty titlebar
        // space stays draggable. The wrappers must size to their content (no
        // `flex_1`), or they'd cover the whole bar and leave nothing to drag.
        // Add future titlebar buttons the same way.
        TitleBar::new()
            .h(px(TITLE_BAR_HEIGHT))
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
                    // Sized to the sidebar column so these controls line up
                    // with it, but shrinkable and clipped: on a narrow window
                    // the alignment is worth less than keeping the window
                    // controls on screen, so this block gives up width before
                    // anything to its right does.
                    .w(px(
                        self.sidebar.width - ui::composition::FLOATING_SURFACE_SIDE_INSET
                    ))
                    .min_w_0()
                    .overflow_hidden()
                    .gap(px(TITLE_BAR_BUTTON_GAP))
                    .child(
                        div()
                            .flex_none()
                            .occlude()
                            .child(self.render_app_menu_button(cx)),
                    )
                    .child(
                        div().flex_none().occlude().child(
                            Button::new("toggle-sidebar")
                                .ghost()
                                .size(px(TITLE_BAR_BUTTON))
                                .icon(if self.sidebar.collapsed {
                                    SideBarIcon::Expand
                                } else {
                                    SideBarIcon::Collapse
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.on_toggle_sidebar(&ToggleSidebar, window, cx)
                                })),
                        ),
                    )
                    // Left in the drag region: naming the application is not a
                    // control, so the pointer keeps the whole strip to move
                    // the window by. It is also the one thing here that gives
                    // up width: this zone is sized to the sidebar and clipped,
                    // and a name pushing a control out of the window is a
                    // worse trade than a truncated name.
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .pl(px(TITLE_BAR_WORDMARK_INSET))
                            .text_size(px(TITLE_BAR_WORDMARK_TEXT))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::new_static("NiumaTerm")),
                    )
                    // The jump controls close the zone from its trailing edge.
                    .child(div().flex_1().min_w_0())
                    // Stays out of the chrome while every background tab is
                    // caught up; there is nowhere for it to jump to then.
                    .children(self.next_ready_tab(cx).is_some().then(|| {
                        div().flex_none().occlude().child(
                            Button::new("next-ready-tab")
                                .ghost()
                                .size(px(TITLE_BAR_BUTTON))
                                .icon(IconName::Bell)
                                .tooltip(i18n("shell-next-ready-tab"))
                                // The target is picked on the click rather
                                // than captured here, so a tab that went ready
                                // (or was closed) since this frame is still
                                // reached by the very next click.
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let Some((workspace_index, tab_index)) =
                                        this.next_ready_tab(cx)
                                    else {
                                        return;
                                    };

                                    this.jump_to_tab(workspace_index, tab_index, window, cx);
                                })),
                        )
                    }))
                    // Same bargain for work still in flight: nothing to jump
                    // to while every tab is idle.
                    .children(self.next_busy_tab(cx).is_some().then(|| {
                        div()
                            .flex_none()
                            .occlude()
                            .relative()
                            .child(
                                Button::new("next-busy-tab")
                                    .ghost()
                                    .size(px(TITLE_BAR_BUTTON))
                                    .icon(NextBusyTabIcon)
                                    .tooltip(i18n("shell-next-busy-tab"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let Some((workspace_index, tab_index)) =
                                            this.next_busy_tab(cx)
                                        else {
                                            return;
                                        };

                                        this.jump_to_tab(workspace_index, tab_index, window, cx);
                                    })),
                            )
                            // Ringed in the bar's own background so the mark
                            // stays legible over whatever the icon under it
                            // happens to be.
                            .child(
                                div()
                                    .absolute()
                                    .top(px(2.))
                                    .right(px(2.))
                                    .size(px(TITLE_BAR_BADGE))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(cx.theme().title_bar)
                                    .bg(cx.theme().warning),
                            )
                    })),
            )
            // The container keeps the title-bar drag area. Tabs and the
            // new-tab button block only their own bounds.
            .child(
                div()
                    .flex_1()
                    // A floor rather than `min_w_0`: without it flexbox drains
                    // this zero-basis column to nothing before squeezing its
                    // neighbours, and a zero-width strip cannot be scrolled
                    // back into view. The strip's own horizontal scroll takes
                    // over once the tabs no longer fit this width.
                    .min_w(px(TAB_STRIP_MIN_WIDTH))
                    .h_full()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .map(|this| match vertical_tabs {
                        true => this.child(self.render_session_heading(cx)),
                        false => this.child(tab_bar),
                    }),
            )
            .child(
                h_flex()
                    // The window controls sit to the right of this group, so
                    // any width it concedes would be reclaimed by the tab
                    // strip and push them off the window.
                    .flex_none()
                    .child(div().occlude().child(self.git_status.clone()))
                    // The sidebar itself stays reachable through the
                    // `ToggleGitSidebar` action while the button is hidden.
                    .children(
                        cx.global::<AppSettings>()
                            .show_git_status_on_title_bar
                            .then(|| {
                                div().occlude().child(
                                    Toggle::new("toggle-git-sidebar")
                                        .ghost()
                                        .checked(self.right_panel_shows(RightPanelKind::Git, cx))
                                        .icon(GitIcon)
                                        .on_click(cx.listener(|this, _: &bool, window, cx| {
                                            this.on_toggle_git_sidebar(
                                                &ToggleGitSidebar,
                                                window,
                                                cx,
                                            )
                                        })),
                                )
                            }),
                    )
                    // Each control gets its own occluding wrapper: a shared one
                    // would stack them, because the wrapper is a column.
                    // The workflow control stays out of the chrome until a run
                    // exists to look at.
                    .children(self.workflows_seen().then(|| {
                        // Scoped to the active tab, because activating the
                        // control opens that tab's runs.
                        let running = self
                            .active_agent()
                            .map(|pane| pane.read(cx).running_workflow_agents())
                            .unwrap_or(0);

                        div()
                            .occlude()
                            .child(self.render_workflows_button(running, cx))
                    }))
                    // The background-task control stays out of the chrome until
                    // a tab has spawned a child to look at.
                    .children(if self.background_tasks_seen() {
                        // The history flag is window-wide, so the active Agent
                        // gate keeps this Agent-only control off terminal tabs.
                        self.active_agent().map(|pane| {
                            // Scoped to the active tab, because activating the
                            // control opens that tab's children.
                            let running = pane.read(cx).running_background_tasks();

                            div()
                                .occlude()
                                .child(self.render_background_tasks_button(running, cx))
                        })
                    } else {
                        None
                    }),
            )
    }

    /// The leading control of the title bar. It carries the commands that have
    /// no chrome of their own; anything with a visible button of its own stays
    /// on that button rather than being listed here as well.
    fn render_app_menu_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = cx.entity();

        ui::modern_dropdown(
            Button::new("app-menu")
                .ghost()
                .size(px(TITLE_BAR_BUTTON))
                .icon(IconName::Menu)
                .tooltip(i18n("shell-app-menu"))
                .aria_label(i18n("shell-app-menu")),
            move |menu, _, cx| app_menu(menu, &shell, cx),
        )
    }

    /// What the title bar names in the vertical tab-bar style, where the strip
    /// that would otherwise fill this space lives in the sidebar: the session
    /// on screen, and the branch its working directory is on.
    fn render_session_heading(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let branch = self
            .git_model
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.branch.clone());

        h_flex()
            .min_w_0()
            .gap(px(TITLE_BAR_HEADING_GAP))
            .items_center()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(TITLE_BAR_HEADING_TEXT))
                    .text_color(cx.theme().muted_foreground)
                    .child(self.active_tab_title()),
            )
            .children(branch.map(|branch| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .rounded(px(TITLE_BAR_CHIP_RADIUS))
                    .px(px(TITLE_BAR_CHIP_PADDING_X))
                    .py(px(TITLE_BAR_CHIP_PADDING_Y))
                    .bg(cx.theme().muted)
                    .text_size(px(TITLE_BAR_CHIP_TEXT))
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::GitBranch).size(px(TITLE_BAR_CHIP_ICON)))
                    .child(branch)
            }))
    }

    /// Upper-right `Workflows` control, revealed once a run exists. It carries
    /// the number of agents running right now, which is the one thing about a
    /// workflow worth watching without opening the view; a run with nothing in
    /// flight shows the icon alone rather than a zero.
    fn render_workflows_button(&self, running: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let label = i18n("workflows-running-agents").replace("{count}", &running.to_string());

        Toggle::new("toggle-workflows")
            .ghost()
            .checked(self.right_panel_shows(RightPanelKind::Workflows, cx))
            // Matches the gap a Button puts between its icon and label; the
            // toggle centres its children without one.
            .gap_2()
            .icon(IconName::LayoutDashboard)
            .when(running > 0, |toggle| toggle.label(running.to_string()))
            .tooltip(label)
            .on_click(cx.listener(|this, _: &bool, window, cx| {
                this.on_toggle_workflows(&ToggleWorkflows, window, cx)
            }))
    }

    /// Upper-right `Background Tasks` control, revealed once a tab has spawned
    /// a child agent. It carries the number of child agents running right now;
    /// a session with none in flight shows the icon alone rather than a zero.
    /// The `ToggleBackgroundTasks` action still reaches the view while the
    /// control is hidden.
    fn render_background_tasks_button(
        &self,
        running: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = match running {
            0 => i18n("tasks-background-title").to_string(),
            _ => i18n("tasks-background-running-agents").replace("{count}", &running.to_string()),
        };

        Toggle::new("toggle-background-tasks")
            .ghost()
            .checked(self.right_panel_shows(RightPanelKind::BackgroundTasks, cx))
            .gap_2()
            .icon(IconName::Bot)
            .when(running > 0, |toggle| toggle.label(running.to_string()))
            .tooltip(label)
            .on_click(cx.listener(|this, _: &bool, window, cx| {
                this.on_toggle_background_tasks(&ToggleBackgroundTasks, window, cx)
            }))
    }
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Titlebar sidebar-toggle icons served by `crate::assets::AppAssets`.
enum SideBarIcon {
    Collapse,
    Expand,
}

impl IconNamed for SideBarIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Collapse => "icons/side-bar-collapse.svg",
            Self::Expand => "icons/side-bar-expand.svg",
        }
        .into()
    }
}

/// The application menu: opening things, then the two application-wide
/// commands. Every entry here is reachable by keyboard as well, so the menu is
/// a place to find them rather than the only way to reach them.
fn app_menu(menu: ModernMenu, shell: &Entity<Shell>, cx: &mut App) -> ModernMenu {
    let window_shell = shell.clone();
    let workspace_shell = shell.clone();
    let settings_shell = shell.clone();

    new_tab_menu(menu, shell, cx)
        .separator()
        .item(i18n("shell-menu-new-window"), move |window, cx| {
            window_shell.update(cx, |this, cx| {
                this.on_new_window(&NewWindow, window, cx);
            });
        })
        .icon(Icon::new(IconName::Frame))
        .item(i18n("shell-workspace-new-title"), move |window, cx| {
            workspace_shell.update(cx, |this, cx| {
                this.on_new_workspace(&NewWorkspace, window, cx);
            });
        })
        .icon(Icon::new(IconName::Folder))
        .separator()
        .item(i18n("shell-workspace-settings-title"), move |window, cx| {
            settings_shell.update(cx, |this, cx| {
                this.on_show_settings(&ShowSettings, window, cx);
            });
        })
        .icon(Icon::new(IconName::Settings))
        .item(i18n("shell-menu-check-updates"), |_, cx| check_now(cx))
        .icon(Icon::new(IconName::ArrowDown))
}

struct GitIcon;

impl IconNamed for GitIcon {
    fn path(self) -> SharedString {
        "icons/git.svg".into()
    }
}

/// Titlebar busy-tab jump icon, backed by the project's `assets/icons/
/// circle-arrow-right.svg`. The arrow is what separates it from the busy
/// spinner drawn on the tabs themselves: this control navigates to that work
/// rather than reporting it.
struct NextBusyTabIcon;

impl IconNamed for NextBusyTabIcon {
    fn path(self) -> SharedString {
        "icons/circle-arrow-right.svg".into()
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

        if let Some(request) = self.pending_agent_resume.take() {
            // The conversation ran in a directory of its own. Where a
            // workspace owns that directory, the reopened tab gets that
            // workspace's whole directory list; otherwise the conversation's
            // own directory is all this tab can honestly claim.
            let workspace =
                exact_match(&self.workspaces.summaries(), path::Path::new(&request.cwd))
                    .and_then(|id| self.workspaces.roots_of(id))
                    .map_or_else(
                        || AgentWorkspace::single(Some(request.cwd.clone())),
                        |roots| agent_workspace(Some(roots)),
                    );

            self.open_agent_tab_in(
                &request.profile,
                workspace,
                Some(RecoveryIdentity::new(
                    AgentKind::from_profile(request.profile.kind),
                    request.session_id,
                )),
                window,
                cx,
            );
        }

        if let Some(tab) = self.pending_agent_close.take() {
            self.request_close_tab(tab, window, cx);
        }

        self.process_native_notifications(cx);

        ui::sync_modern_menu(cx);

        // Any workspace/tab switch re-renders the shell, so this render-time
        // compare-and-set catches every switch path.
        self.sync_git_target(cx);
        self.sync_task_panel_target(cx);

        // The sidebar is always mounted so it can animate its width open/closed.
        let summaries = self.projected_workspace_summaries(cx);

        // Vertical style folds the tab strip into the sidebar as child rows of
        // each workspace, leaving the title bar's strip slot empty.
        let vertical_tabs = cx.global::<AppSettings>().tab_bar_style == TabBarStyle::Vertical;

        let (unread_tabs, busy_agent_tabs) = self.tab_agent_indicators(cx);

        let sidebar_tabs: Vec<Vec<SidebarTab>> = match vertical_tabs {
            false => Vec::new(),
            true => summaries
                .iter()
                .map(|ws| {
                    let Some(tabs) = self.workspaces.tabs_of(ws.id) else {
                        return Vec::new();
                    };
                    let active_id = tabs.active_id();

                    tabs.tabs()
                        .iter()
                        .map(|tab| SidebarTab {
                            id: tab.id(),
                            label: match tab.title().is_empty() {
                                true => SharedString::new_static("PowerShell"),
                                false => tab.title().to_string().into(),
                            },
                            // Every workspace keeps its own active tab, but
                            // only one of them is the tab on screen. Marking
                            // the others would put a selection highlight on
                            // every workspace's list at once.
                            active: ws.active && tab.id() == active_id,
                            unread: unread_tabs.contains(&tab.id()),
                            busy: busy_agent_tabs.contains(&tab.id()),
                            bell: tab.bell(),
                            agent_kind: tab.surface().agent_kind(cx),
                            settings: tab.surface().is_settings(),
                            pending: matches!(tab.surface(), TabSurface::Pending(_)),
                            exited: tab.exited(),
                            progress: tab.progress(),
                            terminal: Self::tab_terminal_activity(tab, cx),
                        })
                        .collect()
                })
                .collect(),
        };

        let sidebar = self.sidebar.render(
            summaries,
            sidebar_tabs,
            self.workspace_rename.as_ref(),
            self.tab_rename.as_ref(),
            SidebarUsage {
                daily: self.token_usage.clone(),
                quotas: self.agent_usage.clone(),
            },
            cx,
        );

        // Re-render the shell whenever the wrapping Root changes (dialog
        // open/close), since the shell draws the dialog layer.
        if !self.root_observed
            && let Some(Some(root)) = window.root::<Root>()
        {
            cx.observe(&root, |_, _, cx| cx.notify()).detach();
            self.root_observed = true;
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

        let tab_bar = match vertical_tabs {
            true => div().into_any_element(),
            false => self.tab_strip.render(
                self.workspaces.active_tabs(),
                &unread_tabs,
                &busy_agent_tabs,
                self.tab_rename.as_ref(),
                cx,
            ),
        };

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
            // A context menu drawn in its own window never takes activation, so
            // that this window keeps its focused backdrop material — which also
            // means it never receives the press or the key that should dismiss
            // it. This window does. Capture phase, because the input still
            // belongs to whatever it was aimed at.
            .capture_any_mouse_down(|_, _, cx| ui::dismiss_modern_menu(cx))
            .capture_key_down(|event: &KeyDownEvent, _, cx| {
                // Capture phase, and propagation stops on anything the menu
                // used: while a menu is up its keys outrank the bindings of
                // whatever still holds focus underneath it.
                if dispatch_modern_menu_key(event, cx) {
                    cx.stop_propagation();
                }
            })
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
            .font(ui::font_with_default_fallback(
                cx.global::<AppSettings>().ui_font_family.clone(),
            ))
            .key_context("Shell");

        Self::bind_actions(shell, cx)
            .children(background_image)
            .child(self.render_title_bar(tab_bar, cx))
            .child(
                div()
                    .flex_1()
                    // Without this the row keeps `min-height: auto` and any
                    // child taller than the window stretches it, which pushes
                    // the bottom-anchored pane content below the viewport and
                    // reads as a blank main area.
                    .min_h_0()
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
                            // Gutters only on the two sides that face other
                            // chrome; the surface runs flush into the window's
                            // right and bottom edges.
                            .pl(px(ui::composition::FLOATING_SURFACE_SIDE_INSET))
                            .pt(px(ui::composition::FLOATING_SURFACE_TOP_INSET))
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
