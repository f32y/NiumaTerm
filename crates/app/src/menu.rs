//! The macOS menu bar.
//!
//! macOS puts an application's commands in the system menu bar whether or not
//! the window draws chrome of its own, and an application without one reads as
//! broken: the bar still shows the previous application's menus. The in-window
//! menu button stays as it is; the two list the same commands, and the bar adds
//! the ones only the system can provide (Services, Hide, the window list).
//!
//! An item's shortcut is read from the keymap, and macOS then reserves that
//! chord for the menu: `performKeyEquivalent:` runs before the key event ever
//! reaches the window. Only chords that are meant to be application commands
//! everywhere belong here, which is why there is no Edit menu -- Command-C and
//! Command-V are answered by whichever surface has focus (a terminal copies its
//! selection, a text field its own), and a menu item would take them away from
//! both.

use gpui::{App, Menu, MenuItem, SystemMenuType, Window, actions};
use nmt_i18n::i18n;

use crate::ui::settings::save_settings;
use crate::ui::{
    CloseTab, NewAgentTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace, PrevTab,
    PrevWorkspace, ShowSettings, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleSidebar,
};
use crate::{open_window_without_a_source, sparkle};

actions!(
    NiumaTerm,
    [
        /// Quit the application.
        Quit,
        /// Hide every window of this application.
        Hide,
        /// Hide every other application's windows.
        HideOthers,
        /// Bring back the windows Hide Others put away.
        ShowAll,
        /// Send the active window to the Dock.
        Minimize,
        /// Toggle the active window between its standard and zoomed size.
        Zoom,
        /// Ask the updater to look for a newer release now.
        CheckForUpdates,
    ]
);

/// Register the handlers for the application-level commands and put the bar up.
///
/// Call after the key bindings are installed; the shortcut shown beside an item
/// comes from the binding registered for its action.
pub(crate) fn install(cx: &mut App) {
    cx.on_action(|_: &Quit, cx: &mut App| {
        if let Some(handle) = cx.active_window() {
            let _ = handle.update(cx, |_, window, cx| {
                if save_settings(window, cx) {
                    cx.quit();
                }
            });
        } else {
            cx.quit();
        }
    });
    cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx: &mut App| cx.unhide_other_apps());
    // NewWindow is answered by the shell, and a focused window stops the action
    // there, so this runs only when no window can take it. Without it the item
    // and the Command-N equivalent macOS reserves for it are dead once the last
    // window closes, which is exactly when a new window is what is wanted.
    cx.on_action(|_: &NewWindow, cx: &mut App| open_window_without_a_source(cx));
    // The window commands act on the window the menu bar belongs to, which is
    // the active one; the menu is disabled outright when there is none.
    cx.on_action(|_: &Minimize, cx: &mut App| with_active_window(cx, Window::minimize_window));
    cx.on_action(|_: &Zoom, cx: &mut App| with_active_window(cx, Window::zoom_window));
    // Disabled rather than absent while a check runs, and for a build with no
    // updater, so the item stays where a user learned to look for it.
    cx.on_action(|_: &CheckForUpdates, cx: &mut App| {
        if sparkle::can_check(cx) {
            sparkle::check_now(cx);
        }
    });

    refresh(cx);
}

/// Rebuild the bar in the language that is now active.
///
/// The bar is built once from translated strings and then held by AppKit, so a
/// live language switch leaves it in the previous language until it is replaced.
pub(crate) fn refresh(cx: &mut App) {
    cx.set_menus(menus());
}

/// Run `command` against the window the menu bar belongs to.
///
/// The bar has no window of its own, and every one of these commands is about
/// the one on screen; with none open there is nothing to act on.
fn with_active_window(cx: &mut App, command: impl FnOnce(&Window)) {
    let Some(handle) = cx.active_window() else {
        return;
    };

    let _ = handle.update(cx, |_, window, _| command(window));
}

fn menus() -> Vec<Menu> {
    vec![
        // Named for the application because macOS shows the first menu's name
        // in bold as the application menu.
        Menu::new("NiumaTerm").items([
            MenuItem::action(i18n("menu-check-for-updates"), CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action(i18n("menu-settings"), ShowSettings),
            MenuItem::separator(),
            MenuItem::os_submenu(i18n("menu-services"), SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(i18n("menu-hide"), Hide),
            MenuItem::action(i18n("menu-hide-others"), HideOthers),
            MenuItem::action(i18n("menu-show-all"), ShowAll),
            MenuItem::separator(),
            MenuItem::action(i18n("menu-quit"), Quit),
        ]),
        Menu::new(i18n("menu-shell")).items([
            MenuItem::action(i18n("shell-menu-new-tab"), NewTab),
            MenuItem::action(i18n("shell-menu-new-agent-tab"), NewAgentTab),
            MenuItem::action(i18n("shell-menu-new-window"), NewWindow),
            MenuItem::action(i18n("shell-workspace-new-title"), NewWorkspace),
            MenuItem::separator(),
            MenuItem::action(i18n("menu-close-tab"), CloseTab),
        ]),
        Menu::new(i18n("menu-view")).items([
            MenuItem::action(i18n("menu-toggle-sidebar"), ToggleSidebar),
            MenuItem::separator(),
            MenuItem::action(i18n("menu-split-up"), SplitUp),
            MenuItem::action(i18n("menu-split-down"), SplitDown),
            MenuItem::action(i18n("menu-split-left"), SplitLeft),
            MenuItem::action(i18n("menu-split-right"), SplitRight),
            // AppKit adds Enter Full Screen to a menu it recognizes as the
            // View menu, so there is no item for it here.
        ]),
        // AppKit fills in the list of open windows for a menu it recognizes by
        // the name "Window", which is what the English catalog calls this one.
        Menu::new(i18n("menu-window")).items([
            MenuItem::action(i18n("menu-minimize"), Minimize),
            MenuItem::action(i18n("menu-zoom"), Zoom),
            MenuItem::separator(),
            MenuItem::action(i18n("menu-next-tab"), NextTab),
            MenuItem::action(i18n("menu-previous-tab"), PrevTab),
            MenuItem::separator(),
            MenuItem::action(i18n("menu-next-workspace"), NextWorkspace),
            MenuItem::action(i18n("menu-previous-workspace"), PrevWorkspace),
        ]),
    ]
}
