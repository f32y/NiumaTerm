use gpui::{Context, Entity};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::{Icon, IconName, Sizable as _};
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

/// The title-bar and sidebar entry points share one menu so both tab layouts
/// expose terminal and agent profiles in the same order.
pub(in crate::ui) fn new_tab_menu(
    mut menu: PopupMenu,
    shell: &Entity<Shell>,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for profile in cx.global::<AppSettings>().profiles.clone() {
        let shell_cmd = profile.shell.trim().to_string();
        if shell_cmd.is_empty() {
            continue;
        }

        let args: Vec<String> = profile
            .args
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let item_shell = shell.clone();

        menu = menu.item(
            PopupMenuItem::new(profile.name.clone())
                .icon(tab_icon(None, false))
                .on_click(move |_, window, cx| {
                    let launch = (Some(shell_cmd.clone()), args.clone());
                    item_shell.update(cx, |this, cx| this.open_profile_tab(launch, window, cx));
                }),
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
