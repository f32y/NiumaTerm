#[cfg(test)]
mod tests;

use app::design::TITLE_BAR_HEIGHT;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Div, Entity, SharedString, div, px};
use gpui_component::modern_menu::ModernMenu;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IconNamed, TitleBar, h_flex,
};
use nmt_config::appearance::TabShape;
use rust_i18n::t;

use crate::ui::composition::{
    FLOATING_SURFACE_SIDE_INSET, TOOLBAR_BUTTON_SIZE, toolbar_button, toolbar_toggle,
};
use crate::ui::git_status::{GitStatusModel, GitStatusView};
use crate::ui::platform_style::{Host, PlatformStyle as _};
use crate::ui::shell::{ToggleBackgroundTasks, ToggleGitSidebar, ToggleWorkflows};
use crate::ui::{
    AppSettings, AppWindow, NewWindow, NewWorkspace, ShowSettings, ToggleSidebar, modern_dropdown,
};
#[cfg(windows)]
use crate::update::check;

/// The window's title bar: the app menu and navigation controls over the
/// sidebar, the tab strip or session heading in the middle, and the panel
/// toggles at the trailing edge. It owns the `+N -M` git summary it shows;
/// everything else it draws comes in with each render.
pub(crate) struct WindowTitleBar {
    /// Titlebar `+N -M` indicator (self-gating on its setting).
    git_status: Entity<GitStatusView>,
}

/// What the title bar shows on one render.
pub(crate) struct TitleBarInputs {
    /// Width of the sidebar below the bar, zero while it is collapsed.
    pub(crate) sidebar_width: f32,

    pub(crate) sidebar_collapsed: bool,

    pub(crate) center: TitleCenter,

    /// Whether the ready-tab and busy-tab jumps currently have a target.
    pub(crate) has_ready_tab: bool,

    pub(crate) has_busy_tab: bool,

    /// Whether the active tab is the Git tab the git toggle opens.
    pub(crate) git_tab_active: bool,

    /// The workflow and background-task toggles, absent until each has
    /// something to show.
    pub(crate) workflows: Option<PanelToggle>,

    pub(crate) background_tasks: Option<PanelToggle>,
}

/// The middle of the bar.
pub(crate) enum TitleCenter {
    /// The horizontal tab strip.
    Tabs(AnyElement),
    /// Vertical tabs move the strip into the sidebar, which leaves the middle
    /// of the bar free to name the session on screen instead: its title, and
    /// the branch its working directory is on.
    Heading {
        title: SharedString,
        branch: Option<SharedString>,
    },
}

/// A trailing panel toggle: how much work its panel reports running, and
/// whether that panel is open.
#[derive(Clone, Copy)]
pub(crate) struct PanelToggle {
    pub(crate) running: usize,
    pub(crate) open: bool,
}

/// Width the tab strip keeps once the title bar runs out of room: about one
/// truncated tab plus the new-tab button, so the strip stays visible and its
/// horizontal scroll stays reachable at the window's minimum width.
const TAB_STRIP_MIN_WIDTH: f32 = 120.0;

const TITLE_BAR_BUTTON_GAP: f32 = 4.0;

/// Four controls, three internal gaps, and a trailing gap stay reachable
/// before the first tab, including at the sidebar's drag limit.
pub(crate) const TITLE_BAR_CONTROLS_WIDTH: f32 = 4.0 * (TOOLBAR_BUTTON_SIZE + TITLE_BAR_BUTTON_GAP);

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

impl WindowTitleBar {
    pub(crate) fn new(git_model: Entity<GitStatusModel>, cx: &mut Context<AppWindow>) -> Self {
        Self {
            git_status: cx.new(|cx| GitStatusView::new(git_model, cx)),
        }
    }

    pub(crate) fn render(&self, inputs: TitleBarInputs, cx: &mut Context<AppWindow>) -> TitleBar {
        let TitleBarInputs {
            sidebar_width,
            sidebar_collapsed,
            center,
            has_ready_tab,
            has_busy_tab,
            git_tab_active,
            workflows,
            background_tasks,
        } = inputs;

        let appearance = &cx.global::<AppSettings>().config().appearance;
        let tab_shape = appearance.tab_shape;
        let show_git_toggle = appearance.show_git_status_on_title_bar;

        // The leading region ends on the sidebar's edge, measured from where
        // the bar's content starts, so it shrinks by the host's leading inset.
        let leading_width =
            (sidebar_width + FLOATING_SURFACE_SIDE_INSET - Host::TITLE_BAR_LEADING_INSET).max(0.0);

        // Interactive chrome lives in the titlebar but is wrapped in
        // `occlude()`: that blocks the drag hitbox beneath it, so Windows
        // treats these regions as client (clickable) while the empty titlebar
        // space stays draggable. The wrappers must size to their content (no
        // `flex_1`), or they'd cover the whole bar and leave nothing to drag.
        // Add future titlebar buttons the same way.
        TitleBar::new()
            .h(px(TITLE_BAR_HEIGHT))
            .map(Host::title_bar)
            // The default X calls `remove_window()` directly (no
            // WM_CLOSE), skipping `on_window_should_close` — so the
            // shared close confirmation is handled here too.
            .on_close_window(cx.listener(|this, _, window, cx| {
                if this.confirm_window_close(window, cx) {
                    window.remove_window();
                }
            }))
            .child(
                title_bar_leading_region(leading_width)
                    .child(div().flex_none().occlude().child(app_menu_button(cx)))
                    .child(
                        div().flex_none().occlude().child(
                            toolbar_button("toggle-sidebar")
                                .icon(if sidebar_collapsed {
                                    SideBarIcon::Expand
                                } else {
                                    SideBarIcon::Collapse
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.on_toggle_sidebar(&ToggleSidebar, window, cx)
                                })),
                        ),
                    )
                    // Anchored to the leading edge behind two fixed-width
                    // controls, so their screen position never moves with the
                    // sidebar width or the session title: repeated clicks can
                    // cycle through targets without the pointer chasing them.
                    // Both stay mounted and go disabled when there is nowhere
                    // to jump, which is what keeps that position stable.
                    .child(
                        div().flex_none().occlude().child(
                            toolbar_button("next-ready-tab")
                                .icon(IconName::Bell)
                                .tooltip(t!("shell-next-ready-tab"))
                                .disabled(!has_ready_tab)
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
                        ),
                    )
                    .child(
                        div().flex_none().occlude().child(
                            toolbar_button("next-busy-tab")
                                .icon(NextBusyTabIcon)
                                .tooltip(t!("shell-next-busy-tab"))
                                .disabled(!has_busy_tab)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let Some((workspace_index, tab_index)) = this.next_busy_tab(cx)
                                    else {
                                        return;
                                    };

                                    this.jump_to_tab(workspace_index, tab_index, window, cx);
                                })),
                        ),
                    )
                    // Absorbs the leftover width so the controls stay packed
                    // against the leading edge.
                    .child(div().flex_1().min_w_0()),
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
                    .items_end()
                    .map(|this| match (center, tab_shape) {
                        (TitleCenter::Heading { title, branch }, _) => this
                            .map(Host::session_heading_slot)
                            .child(session_heading(title, branch, cx)),
                        // Pills float apart from the content, so they center
                        // in the bar. Attached tabs keep the bottom edge they
                        // share with the content below.
                        (TitleCenter::Tabs(tab_bar), TabShape::Rounded) => {
                            this.items_center().child(tab_bar)
                        }
                        (TitleCenter::Tabs(tab_bar), TabShape::Attached) => this.child(tab_bar),
                    }),
            )
            .child(title_bar_git_summary().child(self.git_status.clone()))
            .child(
                title_bar_trailing_region()
                    // The sidebar itself stays reachable through the
                    // `ToggleGitSidebar` action while the button is hidden.
                    .children(show_git_toggle.then(|| {
                        div().flex_none().occlude().child(
                            toolbar_toggle("toggle-git-sidebar")
                                .checked(git_tab_active)
                                .icon(GitIcon)
                                .on_click(cx.listener(|this, _: &bool, window, cx| {
                                    this.on_toggle_git_sidebar(&ToggleGitSidebar, window, cx)
                                })),
                        )
                    }))
                    // Each control gets its own occluding wrapper: a shared one
                    // would stack them, because the wrapper is a column.
                    .children(workflows.map(|toggle| {
                        div()
                            .flex_none()
                            .occlude()
                            .child(workflows_button(toggle, cx))
                    }))
                    .children(background_tasks.map(|toggle| {
                        div()
                            .flex_none()
                            .occlude()
                            .child(background_tasks_button(toggle, cx))
                    })),
            )
    }
}

/// The leading control of the title bar. It carries the commands that have
/// no chrome of their own; anything with a visible button of its own stays
/// on that button rather than being listed here as well.
fn app_menu_button(cx: &mut Context<AppWindow>) -> impl IntoElement {
    let shell = cx.entity();

    modern_dropdown(
        toolbar_button("app-menu")
            .icon(IconName::Menu)
            .tooltip(t!("shell-app-menu"))
            .accessibility_label(t!("shell-app-menu")),
        move |menu, _, cx| app_menu(menu, &shell, cx),
    )
}

/// What the title bar names in the vertical tab-bar style, where the strip
/// that would otherwise fill this space lives in the sidebar: the session
/// on screen, and the branch its working directory is on.
fn session_heading(
    title: SharedString,
    branch: Option<SharedString>,
    cx: &App,
) -> impl IntoElement {
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
                .child(title),
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
/// the number of agents running right now in the active tab, which is the one
/// thing about a workflow worth watching without opening the view; a run with
/// nothing in flight shows the icon alone rather than a zero.
fn workflows_button(toggle: PanelToggle, cx: &mut Context<AppWindow>) -> impl IntoElement {
    let PanelToggle { running, open } = toggle;

    let label = t!("workflows-running-agents", count = running).into_owned();

    toolbar_toggle("toggle-workflows")
        .checked(open)
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
/// background work. It carries the number of tasks running right now in the
/// active tab; a session with none in flight shows the icon alone rather than
/// a zero. The `ToggleBackgroundTasks` action still reaches the view while the
/// control is hidden.
fn background_tasks_button(toggle: PanelToggle, cx: &mut Context<AppWindow>) -> impl IntoElement {
    let PanelToggle { running, open } = toggle;

    let label = match running {
        0 => t!("tasks-background-title").to_string(),
        _ => t!("tasks-background-running-count", count = running).into_owned(),
    };

    toolbar_toggle("toggle-background-tasks")
        .checked(open)
        .gap_2()
        .icon(IconName::Bot)
        .when(running > 0, |toggle| toggle.label(running.to_string()))
        .tooltip(label)
        .on_click(cx.listener(|this, _: &bool, window, cx| {
            this.on_toggle_background_tasks(&ToggleBackgroundTasks, window, cx)
        }))
}

fn title_bar_leading_region(width: f32) -> Div {
    // Sidebar alignment yields to the tab strip on narrow windows, while
    // the minimum width keeps every leading control reachable.
    h_flex()
        .w(px(width))
        .min_w(px(TITLE_BAR_CONTROLS_WIDTH))
        .flex_initial()
        .overflow_hidden()
        .gap(px(TITLE_BAR_BUTTON_GAP))
}

fn title_bar_trailing_region() -> Div {
    // The host's trailing inset keeps toggled and hovered controls inside a
    // curved window edge; a host whose caption controls follow this group
    // reserves that room itself.
    h_flex().flex_none().mr(px(Host::TITLE_BAR_TRAILING_INSET))
}

fn title_bar_git_summary() -> Div {
    // Counts can yield space before the buttons or tab strip become unreachable.
    div().flex_initial().min_w_0().overflow_hidden().occlude()
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
fn app_menu(menu: ModernMenu, shell: &Entity<AppWindow>, _cx: &mut App) -> ModernMenu {
    let window_shell = shell.clone();
    let workspace_shell = shell.clone();
    let settings_shell = shell.clone();

    let menu = menu
        .item(t!("shell-menu-new-window"), move |window, cx| {
            window_shell.update(cx, |this, cx| {
                this.on_new_window(&NewWindow, window, cx);
            });
        })
        .icon(Icon::new(IconName::Frame))
        .item(t!("shell-workspace-new-title"), move |window, cx| {
            workspace_shell.update(cx, |this, cx| {
                this.on_new_workspace(&NewWorkspace, window, cx);
            });
        })
        .icon(Icon::new(IconName::Folder))
        .separator()
        .item(t!("shell-workspace-settings-title"), move |window, cx| {
            settings_shell.update(cx, |this, cx| {
                this.on_show_settings(&ShowSettings, window, cx);
            });
        })
        .icon(Icon::new(IconName::Settings));

    // Only a build that can replace itself offers to check.
    #[cfg(windows)]
    let menu = menu
        .item(t!("shell-menu-check-updates"), |_, cx| check(cx))
        .icon(Icon::new(IconName::ArrowDown));

    menu
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
