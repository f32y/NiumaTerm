//! The application's key bindings.
//!
//! Windows and macOS get separate tables rather than one table with a
//! substituted modifier. A terminal has to leave Control to the shell, so on
//! Windows every application chord is pushed onto Ctrl-Shift and Ctrl-Alt;
//! macOS has a Command key that no terminal program claims and puts the same
//! commands on the plain Command chords its users already know. Substituting
//! one modifier for the other would produce Cmd-Shift-T for a new tab, which is
//! not what any macOS application does.

use app::terminal_tab::view::{NextBlock, PreviousBlock, SendShiftTab, SendTab};
use gpui::{App, KeyBinding};

#[cfg(target_os = "macos")]
use crate::menu::{Hide, HideOthers, Minimize, Quit};
#[cfg(windows)]
use crate::ui::NewRemoteTab;
use crate::ui::{
    CloseTab, NewAgentTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace, PrevTab,
    PrevWorkspace, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, ShowSettings,
    SplitDown, SplitLeft, SplitRight, SplitUp, ToggleSidebar,
};

/// Install the bindings for this platform.
///
/// Runs before the menu bar is built: a menu item takes the shortcut it
/// displays, and the key equivalent the platform then reserves for it, from the
/// binding already registered for its action.
pub(crate) fn bind(cx: &mut App) {
    cx.bind_keys(shell_bindings());
    cx.bind_keys(terminal_bindings());

    #[cfg(target_os = "macos")]
    cx.bind_keys(application_bindings());
}

/// Chords the window chrome answers: tabs, workspaces, panes and settings.
#[cfg(windows)]
fn shell_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("ctrl-shift-t", NewTab, Some("Shell")),
        KeyBinding::new("ctrl-shift-w", CloseTab, Some("Shell")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Shell")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Shell")),
        KeyBinding::new("ctrl-shift-n", NewWorkspace, Some("Shell")),
        KeyBinding::new("ctrl-alt-n", NewWindow, Some("Shell")),
        KeyBinding::new("ctrl-pagedown", NextWorkspace, Some("Shell")),
        KeyBinding::new("ctrl-pageup", PrevWorkspace, Some("Shell")),
        KeyBinding::new("ctrl-shift-b", ToggleSidebar, Some("Shell")),
        KeyBinding::new("ctrl-,", ShowSettings, Some("Shell")),
        KeyBinding::new("ctrl-shift-r", NewRemoteTab, Some("Shell")),
        KeyBinding::new("ctrl-shift-a", NewAgentTab, Some("Shell")),
        // Split-pane creation and keyboard resize. These consume the xterm
        // `\x1b[1;7A..D` / `\x1b[1;4A..D` arrow sequences before the terminal
        // encodes them (accepted conflict, see the terminal-split-panes
        // change).
        KeyBinding::new("ctrl-alt-up", SplitUp, Some("Shell")),
        KeyBinding::new("ctrl-alt-down", SplitDown, Some("Shell")),
        KeyBinding::new("ctrl-alt-left", SplitLeft, Some("Shell")),
        KeyBinding::new("ctrl-alt-right", SplitRight, Some("Shell")),
        KeyBinding::new("alt-shift-up", ResizePaneUp, Some("Shell")),
        KeyBinding::new("alt-shift-down", ResizePaneDown, Some("Shell")),
        KeyBinding::new("alt-shift-left", ResizePaneLeft, Some("Shell")),
        KeyBinding::new("alt-shift-right", ResizePaneRight, Some("Shell")),
    ]
}

/// The same commands on the chords macOS applications use for them. Remote
/// sessions are absent because the host they connect to is Windows-only.
#[cfg(target_os = "macos")]
fn shell_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-t", NewTab, Some("Shell")),
        KeyBinding::new("cmd-w", CloseTab, Some("Shell")),
        // Both spellings of tab cycling: the bracket chords are what a tabbed
        // macOS window offers, and Ctrl-Tab is what anyone arriving from
        // another terminal reaches for.
        KeyBinding::new("cmd-shift-]", NextTab, Some("Shell")),
        KeyBinding::new("cmd-shift-[", PrevTab, Some("Shell")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Shell")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Shell")),
        KeyBinding::new("cmd-n", NewWindow, Some("Shell")),
        KeyBinding::new("cmd-shift-n", NewWorkspace, Some("Shell")),
        // Apple keyboards have no PageUp/PageDown, so switching workspaces
        // rides the arrow keys rather than the page keys the Windows table
        // uses. Control-Command keeps it clear of the split chords below.
        KeyBinding::new("ctrl-cmd-right", NextWorkspace, Some("Shell")),
        KeyBinding::new("ctrl-cmd-left", PrevWorkspace, Some("Shell")),
        KeyBinding::new("cmd-shift-b", ToggleSidebar, Some("Shell")),
        KeyBinding::new("cmd-,", ShowSettings, Some("Shell")),
        KeyBinding::new("cmd-shift-a", NewAgentTab, Some("Shell")),
        // Splitting takes Command-Option rather than the Windows table's
        // Control-Option, which a macOS terminal encodes and sends to the
        // shell as an escape sequence.
        KeyBinding::new("cmd-alt-up", SplitUp, Some("Shell")),
        KeyBinding::new("cmd-alt-down", SplitDown, Some("Shell")),
        KeyBinding::new("cmd-alt-left", SplitLeft, Some("Shell")),
        KeyBinding::new("cmd-alt-right", SplitRight, Some("Shell")),
        KeyBinding::new("alt-shift-up", ResizePaneUp, Some("Shell")),
        KeyBinding::new("alt-shift-down", ResizePaneDown, Some("Shell")),
        KeyBinding::new("alt-shift-left", ResizePaneLeft, Some("Shell")),
        KeyBinding::new("alt-shift-right", ResizePaneRight, Some("Shell")),
    ]
}

/// Chords a focused terminal answers itself.
///
/// Tab and Shift-Tab belong to the shell (completion) while the terminal is
/// focused, but `Root` binds them to focus traversal and key bindings dispatch
/// before the pane's `on_key_down` listener; the deeper `Terminal` context wins
/// over `Root`.
#[cfg(windows)]
fn terminal_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("tab", SendTab, Some("Terminal")),
        KeyBinding::new("shift-tab", SendShiftTab, Some("Terminal")),
        // Move between command blocks.
        KeyBinding::new("ctrl-shift-up", PreviousBlock, Some("Terminal")),
        KeyBinding::new("ctrl-shift-down", NextBlock, Some("Terminal")),
    ]
}

/// The same terminal chords on Command-Shift. Control-Shift is left alone here
/// because a macOS terminal still encodes those as control sequences for the
/// program on the other end of the PTY.
#[cfg(target_os = "macos")]
fn terminal_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("tab", SendTab, Some("Terminal")),
        KeyBinding::new("shift-tab", SendShiftTab, Some("Terminal")),
        KeyBinding::new("cmd-shift-up", PreviousBlock, Some("Terminal")),
        KeyBinding::new("cmd-shift-down", NextBlock, Some("Terminal")),
    ]
}

/// The chords macOS reserves for every application, bound with no context so
/// they answer wherever focus sits.
#[cfg(target_os = "macos")]
fn application_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]
}
