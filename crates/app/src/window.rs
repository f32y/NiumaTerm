use gpui::{
    AnyView, AnyWindowHandle, App, AppContext, Bounds, Global, Styled as _, TitlebarOptions,
    WeakEntity, WindowAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowId,
    WindowOptions, point, px, size, transparent_black,
};
use gpui_component::{Root, Theme as ComponentTheme};
use nmt_config::local_state::{SessionState, WindowLocalState, WindowState};
use nmt_i18n::i18n;

use crate::ui::{self, Shell};

/// One terminal window's runtime state: last-known geometry (stashed by the
/// shell's bounds observer) and the current session snapshot (stashed on
/// workspace/tab changes). Flushed to `local_state.toml` on quit.
pub(crate) struct AppWindow {
    pub(crate) bounds: Option<WindowState>,
    pub(crate) session: Option<SessionState>,
    /// Expanded sidebar width; stashed by the sidebar resize drag.
    pub(crate) sidebar_width: Option<f32>,
    /// CLI `new_window` target directory: the shell skips session restore and
    /// seeds one workspace rooted here. Never persisted.
    pub(crate) initial_cwd: Option<String>,
}

/// Live state of every open window, in creation order. In-memory only; the
/// quit hook serializes it to `local_state.toml`.
pub(crate) struct WindowRegistry(pub(crate) Vec<(WindowId, AppWindow)>);

impl Global for WindowRegistry {}

/// One open window's shell, reachable from the CLI dispatch task. Registered
/// by `Shell::new`, pruned alongside `WindowRegistry` on window close.
pub(crate) struct ShellEntry {
    pub(crate) window_id: WindowId,
    pub(crate) handle: AnyWindowHandle,
    pub(crate) shell: WeakEntity<Shell>,
}

pub(crate) struct ShellRegistry(pub(crate) Vec<ShellEntry>);

impl Global for ShellRegistry {}

impl ShellRegistry {
    pub(crate) fn remove(&mut self, id: WindowId) {
        self.0.retain(|entry| entry.window_id != id);
    }
}

/// The window that most recently gained focus; CLI `new_tab`/`activate`
/// target it. `None` until any window activates.
pub(crate) struct LastActiveWindow(pub(crate) Option<WindowId>);

impl Global for LastActiveWindow {}

pub(crate) fn selected_window_appearance(cx: &App) -> WindowAppearance {
    if ComponentTheme::global(cx).is_dark() {
        WindowAppearance::Dark
    } else {
        WindowAppearance::Light
    }
}

impl WindowRegistry {
    pub(crate) fn get(&self, id: WindowId) -> Option<&AppWindow> {
        self.0.iter().find(|(wid, _)| *wid == id).map(|(_, w)| w)
    }

    pub(crate) fn get_mut(&mut self, id: WindowId) -> Option<&mut AppWindow> {
        self.0
            .iter_mut()
            .find(|(wid, _)| *wid == id)
            .map(|(_, w)| w)
    }

    pub(crate) fn remove(&mut self, id: WindowId) {
        self.0.retain(|(wid, _)| *wid != id);
    }
}

/// Narrower than this the title bar cannot hold the tab strip alongside the
/// window controls, and shorter than this a terminal pane stops showing a
/// usable number of rows. Enforced by the platform through WM_GETMINMAXINFO,
/// so it also bounds interactive resize, not just the initial geometry.
pub(crate) const MIN_WINDOW_WIDTH: f32 = 640.0;
const MIN_WINDOW_HEIGHT: f32 = 400.0;

impl AppWindow {
    /// Startup state from one persisted window entry. `restore_session: false`
    /// discards the saved session; the caller persists that cleanup.
    pub(crate) fn from_local_state(state: &WindowLocalState, restore_session: bool) -> Self {
        Self {
            bounds: state.window.clone(),
            session: restore_session.then(|| state.session.clone()).flatten(),
            sidebar_width: state.sidebar_width,
            initial_cwd: None,
        }
    }

    /// Persisted form of this window's state. `save_session: false` drops the
    /// session snapshot.
    pub(crate) fn to_local_state(&self, save_session: bool) -> WindowLocalState {
        WindowLocalState {
            window: self.bounds.clone(),
            session: save_session.then(|| self.session.clone()).flatten(),
            sidebar_width: self.sidebar_width,
        }
    }

    /// Open a terminal window from its initial state: remembered geometry (or
    /// centered default), borderless client decorations, `Root(Shell)`. The
    /// window's registry entry is created before the shell so the shell can
    /// read its own session to restore.
    pub(crate) fn open(cx: &mut App, initial: AppWindow) -> WindowHandle<Root> {
        let window_bounds = match &initial.bounds {
            Some(w) => {
                // WM_GETMINMAXINFO bounds interactive resize only, so geometry
                // saved by an older build (or edited by hand) is clamped here
                // as well; otherwise the window would restore below its
                // minimum and stay there until the user resized it.
                let bounds = Bounds::new(
                    point(px(w.x), px(w.y)),
                    size(
                        px(w.width.max(MIN_WINDOW_WIDTH)),
                        px(w.height.max(MIN_WINDOW_HEIGHT)),
                    ),
                );
                if w.maximized {
                    WindowBounds::Maximized(bounds)
                } else {
                    WindowBounds::Windowed(bounds)
                }
            }
            None => WindowBounds::Windowed(Bounds::centered(None, size(px(960.0), px(620.0)), cx)),
        };

        cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                // Borderless: the app draws its own titlebar (gpui-component
                // `TitleBar`); the Windows backend routes controls/drag/resize.
                window_decorations: Some(WindowDecorations::Client),
                titlebar: Some(TitlebarOptions {
                    title: Some(i18n("app-window-title").into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                window_background: ui::window_background_appearance(cx),
                window_appearance_override: Some(selected_window_appearance(cx)),
                window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
                ..Default::default()
            },
            // Wrap the shell in gpui-component's `Root` so modal/dialog layers
            // render. The shell focuses its active pane on first render.
            move |window, cx| {
                cx.global_mut::<WindowRegistry>()
                    .0
                    .push((window.window_handle().window_id(), initial));
                let shell: AnyView = cx.new(|cx| Shell::new(window, cx)).into();
                // Each top-level region paints the configured alpha once. A
                // background on Root would sit underneath all of them and make
                // the effective opacity higher than the requested value.
                cx.new(|cx| Root::new(shell, window, cx).bg(transparent_black()))
            },
        )
        .expect("open GPUI terminal window")
    }
}

#[cfg(test)]
mod tests {
    use nmt_config::local_state::{SessionState, TabState, WorkspaceState};

    use super::*;

    fn window_state() -> WindowState {
        WindowState {
            x: 1.0,
            y: 2.0,
            width: 800.0,
            height: 600.0,
            maximized: false,
        }
    }

    fn session_state() -> SessionState {
        SessionState {
            active_workspace: 0,
            workspaces: vec![WorkspaceState {
                name: "Workspace 1".into(),
                cwd: Some("C:/Projects/example".into()),
                pinned: false,
                active_tab: 0,
                tabs: vec![TabState {
                    name: None,
                    user_named: false,
                    shell: Some("pwsh.exe".into()),
                    args: vec!["-NoLogo".into()],
                    cwd: Some("C:/Projects/example/repo".into()),
                    agent: None,
                    agent_profile: None,
                    panes: None,
                }],
            }],
        }
    }

    fn remembered() -> WindowLocalState {
        WindowLocalState {
            window: Some(window_state()),
            session: Some(session_state()),
            sidebar_width: Some(220.0),
        }
    }

    #[test]
    fn restore_enabled_loads_session_and_quit_saves_both() {
        let state = AppWindow::from_local_state(&remembered(), true);
        assert_eq!(state.bounds, Some(window_state()));
        assert_eq!(state.session, Some(session_state()));
        assert_eq!(state.to_local_state(true), remembered());
    }

    #[test]
    fn restore_disabled_discards_session_and_quit_skips_session_save() {
        let state = AppWindow::from_local_state(&remembered(), false);
        assert_eq!(state.bounds, Some(window_state()));
        assert_eq!(state.session, None);
        // The startup-cleanup save and the quit save both go through
        // `to_local_state(false)`: geometry kept, session cleared.
        let state = AppWindow {
            session: Some(session_state()),
            ..state
        };
        assert_eq!(
            state.to_local_state(false),
            WindowLocalState {
                window: Some(window_state()),
                session: None,
                sidebar_width: Some(220.0),
            }
        );
    }
}
