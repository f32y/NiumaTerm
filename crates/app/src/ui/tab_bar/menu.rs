use gpui::{Context, Entity, Window};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::{Icon, IconName, Sizable as _};
use nmt_config::profile::Profile;
use nmt_i18n::i18n;

use crate::agent::AgentKind;
use crate::ui::{AppSettings, Shell};

/// The glyph a tab leads with: the agent's own mark on an agent tab, a gear on
/// the settings tab, and a terminal mark otherwise.
pub(in crate::ui) fn tab_icon(agent_kind: Option<AgentKind>, settings: bool) -> Icon {
    match agent_kind {
        Some(kind) => kind.icon(),
        None if settings => Icon::new(IconName::Settings),
        None => Icon::new(IconName::SquareTerminal),
    }
    .xsmall()
}

/// A terminal Profile's launch command: the executable and its arguments.
type LaunchCommand = (Option<String>, Vec<String>);

/// One terminal Profile and workspace-directory pair offered by the `More`
/// submenu.
pub(in crate::ui) struct ProfileRootChoice {
    pub label: String,
    pub launch: LaunchCommand,
    pub cwd: String,
    /// A restored directory the filesystem cannot currently reach stays listed
    /// and disabled, so its absence is visible rather than silent.
    pub enabled: bool,
}

/// The launch command of a terminal Profile, or `None` when the Profile names
/// no executable and therefore has nothing to start.
fn launch_command(profile: &Profile) -> Option<LaunchCommand> {
    let shell = profile.shell.trim().to_string();
    if shell.is_empty() {
        return None;
    }
    let args = profile
        .args
        .split_whitespace()
        .map(str::to_string)
        .collect();
    Some((Some(shell), args))
}

/// Every terminal Profile and attached-directory combination, ordered
/// Profile-major: one Profile's directories run together before the next
/// Profile begins. The Profile is what a user picks first and the directory
/// only narrows it, so grouping the other way would scatter the entries they
/// are scanning for. Each label carries the Profile name and the directory's
/// full path, because two attached directories can share a final component.
pub(in crate::ui) fn profile_root_choices(
    profiles: &[Profile],
    roots: &[(String, bool)],
) -> Vec<ProfileRootChoice> {
    let mut choices = Vec::new();
    for profile in profiles {
        let Some(launch) = launch_command(profile) else {
            continue;
        };
        for (cwd, available) in roots {
            choices.push(ProfileRootChoice {
                label: i18n("tabbar-menu-profile-in-directory")
                    .replace("{profile}", &profile.name)
                    .replace("{path}", cwd),
                launch: launch.clone(),
                cwd: cwd.clone(),
                enabled: *available,
            });
        }
    }
    choices
}

/// The title-bar and sidebar entry points share one menu so both tab layouts
/// expose terminal and agent profiles in the same order.
pub(in crate::ui) fn new_tab_menu(
    mut menu: PopupMenu,
    shell: &Entity<Shell>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let profiles = cx.global::<AppSettings>().profiles.clone();

    for profile in profiles.clone() {
        let Some(launch) = launch_command(&profile) else {
            continue;
        };
        let item_shell = shell.clone();

        menu = menu.item(
            PopupMenuItem::new(profile.name.clone())
                .icon(tab_icon(None, false))
                .on_click(move |_, window, cx| {
                    let launch = launch.clone();
                    item_shell.update(cx, |this, cx| this.open_profile_tab(launch, window, cx));
                }),
        );
    }

    // One snapshot of the active workspace's directories and their last known
    // availability, taken as the menu opens. The re-check runs on the
    // background executor, so a drive that came back reaches the next opening
    // rather than making this one wait on the filesystem.
    let roots = shell.update(cx, |this, cx| {
        this.refresh_root_availability(cx);
        this.active_root_availability()
    });
    if roots.len() > 1 {
        let choices = profile_root_choices(&profiles, &roots);
        let submenu_shell = shell.clone();

        menu = menu.submenu(
            i18n("tabbar-menu-more"),
            window,
            cx,
            move |mut menu, _, _| {
                for choice in &choices {
                    let item_shell = submenu_shell.clone();
                    let launch = choice.launch.clone();
                    let cwd = choice.cwd.clone();

                    menu = menu.item(
                        PopupMenuItem::new(choice.label.clone())
                            .icon(tab_icon(None, false))
                            .disabled(!choice.enabled)
                            .on_click(move |_, window, cx| {
                                let launch = launch.clone();
                                let cwd = cwd.clone();
                                item_shell.update(cx, |this, cx| {
                                    this.open_profile_tab_in_directory(launch, cwd, window, cx)
                                });
                            }),
                    );
                }
                menu
            },
        );
    }

    let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();
    if !agent_profiles.is_empty() {
        menu = menu.separator();
    }

    for (ix, profile) in agent_profiles.into_iter().enumerate() {
        let label = if profile.name.trim().is_empty() {
            i18n("tabbar-menu-agent-profile").replace("{index}", &(ix + 1).to_string())
        } else {
            profile.name.clone()
        };
        let item_shell = shell.clone();

        menu = menu.item(
            PopupMenuItem::new(label)
                .icon(tab_icon(Some(AgentKind::from_profile(profile.kind)), false))
                .on_click(move |_, window, cx| {
                    let profile = profile.clone();
                    item_shell.update(cx, |this, cx| this.open_agent_tab(profile, window, cx));
                }),
        );
    }

    menu
}
