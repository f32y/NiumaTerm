use gpui::{
    AnyView, AnyWindowHandle, App, AppContext, Bounds, Global, Styled as _, TitlebarOptions,
    WeakEntity, WindowAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowId,
    WindowOptions, point, px, size, transparent_black,
};
use gpui_component::{Root, Theme as ComponentTheme};
use nmt_config::local_state::{SessionState, WindowLocalState, WindowState};
use nmt_i18n::i18n;

use crate::ui::{self, Shell};

/// Height of a macOS close/minimize/zoom button, measured from the frame
/// AppKit gives the standard window buttons. Their vertical inset is applied
/// symmetrically from the top of the window, so half the leftover space
/// centers them.
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_HEIGHT: f32 = 14.0;
/// Leading edge shared by the native close button and workspace row fills.
#[cfg(target_os = "macos")]
pub(crate) const TRAFFIC_LIGHT_INSET: f32 = (ui::TITLE_BAR_HEIGHT - TRAFFIC_LIGHT_HEIGHT) / 2.0;

/// Titlebar setup for a shell window. macOS keeps drawing its own window
/// buttons over the transparent titlebar, and AppKit centers them in a 32px
/// strip; this bar is taller, so the buttons are re-anchored to its middle to
/// line up with the controls the bar draws itself. Windows and Linux draw
/// their controls as part of the bar and need no such adjustment.
fn titlebar_options() -> TitlebarOptions {
    #[allow(unused_mut)]
    let mut titlebar = TitlebarOptions {
        title: Some(i18n("app-window-title").into()),
        appears_transparent: true,
        ..Default::default()
    };

    #[cfg(target_os = "macos")]
    {
        // Equal top and leading insets center the close button within the
        // rounded corner instead of retaining the narrower native titlebar inset.
        let inset = px(TRAFFIC_LIGHT_INSET);
        titlebar.traffic_light_position = Some(point(inset, inset));
    }

    titlebar
}

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
                titlebar: Some(titlebar_options()),
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
mod tests;
