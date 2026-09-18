use std::io;

use app::utils::background_write_reply;
use gpui::{App, Global};
use nmt_agent::HookInstallStatus;
use nmt_agent::claude_code::hook as claude;
use nmt_agent::codex::hook as codex;
use tracing::warn;

#[derive(Clone, Copy)]
pub(super) enum Hook {
    Claude,
    Codex,
}

#[derive(Default)]
pub(super) struct HookState {
    pub detected: bool,
    pub status: Option<HookInstallStatus>,
    pub pending: bool,
    generation: u64,
}

#[derive(Default)]
pub(super) struct AgentHooks([HookState; 2]);

impl Global for AgentHooks {}

impl Hook {
    pub(super) fn state(self, cx: &App) -> Option<&HookState> {
        cx.try_global::<AgentHooks>()
            .map(|hooks| &hooks.0[self as usize])
    }

    fn read(self) -> HookState {
        let (detected, status) = match self {
            Self::Claude => (
                claude::settings_path().is_some_and(|path| path.is_file()),
                claude::settings_path().map(|path| claude::hooks_status(&path)),
            ),
            Self::Codex => (
                codex::config_path().is_some_and(|path| path.is_file()),
                codex::hooks_path().map(|path| codex::hooks_status(&path)),
            ),
        };

        HookState {
            detected,
            status,
            ..HookState::default()
        }
    }

    /// Reread the installation status from disk.
    pub(super) fn refresh(self, cx: &mut App) {
        self.reload(None, cx);
    }

    /// Install or remove the hooks, then reread the status they left.
    pub(super) fn set_installed(self, enabled: bool, cx: &mut App) {
        self.reload(Some(enabled), cx);
    }

    fn reload(self, install: Option<bool>, cx: &mut App) {
        let state = &mut cx.default_global::<AgentHooks>().0[self as usize];

        state.generation += 1;
        state.pending = true;

        let generation = state.generation;

        let work = background_write_reply(cx, move || {
            if let Some(enabled) = install {
                let result = self.write(enabled);

                if let Err(error) = result {
                    warn!("failed to update Agent hooks: {error}");
                }
            }

            self.read()
        });

        cx.spawn(async move |cx| {
            let mut loaded = work.await;

            cx.update(|cx| {
                let state = &mut cx.global_mut::<AgentHooks>().0[self as usize];

                if state.generation == generation {
                    loaded.generation = generation;
                    *state = loaded;
                }

                cx.refresh_windows();
            });
        })
        .detach();
    }

    fn write(self, enabled: bool) -> io::Result<()> {
        match self {
            Self::Claude => {
                let path = claude::settings_path()
                    .ok_or_else(|| io::Error::other("missing Claude settings path"))?;

                if enabled {
                    claude::install_hooks(&path)
                } else {
                    claude::uninstall_hooks(&path)
                }
            }
            Self::Codex => {
                let path = codex::hooks_path()
                    .ok_or_else(|| io::Error::other("missing Codex hooks path"))?;

                if enabled {
                    codex::install_hooks(&path)
                } else {
                    codex::uninstall_hooks(&path)
                }
            }
        }
    }
}
