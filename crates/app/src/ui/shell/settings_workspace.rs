//! Settings as a workspace of its own.
//!
//! Settings opens as a workspace rather than a dialog so the window keeps its
//! tabs while it is up, which means it also has to be retired again and the
//! previous workspace restored when the user leaves.

use std::borrow::Cow;

use gpui::{App, AppContext as _, Context, Entity, Task, Window};
use gpui_component::setting::{SelectIndex, Settings, SettingsState};
use rust_i18n::t;

use crate::ui;
use crate::ui::settings::SettingsEditing;
use crate::ui::shell::Shell;

/// Sidebar entry name and tab title of the settings pseudo workspace, in the
/// active language. Looked up at creation time; the entry is never persisted,
/// so a stale-language name cannot leak into local_state.
pub(super) fn settings_title() -> Cow<'static, str> {
    t!("shell-workspace-settings-title")
}

/// Everything the settings surface owns while it is on screen: the page and
/// search state that outlives a repaint, the themes-directory watcher that
/// makes theme edits preview live.
#[derive(Default)]
pub(super) struct SettingsSurface {
    open: Option<OpenSettings>,
}

struct OpenSettings {
    state: Entity<SettingsState>,
    editing: Entity<SettingsEditing>,
    _theme_watcher: Option<Task<()>>,
}

impl SettingsSurface {
    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let state = SettingsState::owned(SelectIndex::default(), window, cx);
        let editing = cx.new(|_| SettingsEditing::default());

        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        cx.observe(&editing, |_, _, cx| cx.notify()).detach();

        let theme_watcher = ui::watch_themes(&editing, cx);

        self.open = Some(OpenSettings {
            state,
            editing,
            _theme_watcher: theme_watcher,
        });
    }

    pub(super) fn retire(&mut self) {
        self.open = None;
    }

    pub(super) fn render(&self, cx: &App) -> Option<Settings> {
        let open = self.open.as_ref()?;

        Some(ui::settings::settings_view(open.editing.clone(), cx).state(open.state.clone()))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, IntoElement, ParentElement as _, Render, TestAppContext,
        VisualTestContext, Window, div,
    };
    use gpui_component::setting::{SelectIndex, SettingsState};

    use crate::ui::settings::{AppSettings, SettingsEditing};
    use crate::ui::shell::settings_workspace::{OpenSettings, SettingsSurface};

    struct SettingsHost(SettingsSurface);

    impl Render for SettingsHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().children(self.0.render(cx))
        }
    }

    #[gpui::test]
    fn closed_settings_release_local_edits_while_another_window_stays_open(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AppSettings::default());
        });

        let mut windows = Vec::new();
        let mut editors = Vec::new();

        for _ in 0..2 {
            let window = cx.update(|cx| {
                cx.open_window(Default::default(), |window, cx| {
                    let editing = cx.new(|_| SettingsEditing::default());

                    editors.push(editing.downgrade());

                    let state = SettingsState::owned(
                        SelectIndex {
                            page_ix: 0,
                            group_ix: Some(1),
                        },
                        window,
                        cx,
                    );

                    cx.new(|_| {
                        SettingsHost(SettingsSurface {
                            open: Some(OpenSettings {
                                state,
                                editing,
                                _theme_watcher: None,
                            }),
                        })
                    })
                })
                .unwrap()
            });

            windows.push(window);
        }

        cx.run_until_parked();

        editors[0]
            .update(cx, |editing, cx| {
                editing.theme_filter = "First window".into();

                #[cfg(windows)]
                {
                    editing.remote_pairing_input = "temporary pairing code".into();
                }

                cx.notify();
            })
            .unwrap();

        cx.update(|cx| {
            assert!(
                editors[1]
                    .upgrade()
                    .unwrap()
                    .read(cx)
                    .theme_filter
                    .is_empty()
            );
        });

        let mut cx = VisualTestContext::from_window(windows[0].into(), cx);

        windows[0]
            .update(&mut cx, |host, _, cx| {
                host.0.retire();

                cx.notify();
            })
            .unwrap();

        // Render state keeps the previous frame alive until the next frame
        // completes, so both retained frames must stop referencing the page.
        for _ in 0..2 {
            windows[0].update(&mut cx, |_, _, cx| cx.notify()).unwrap();
            cx.run_until_parked();
            cx.refresh().unwrap();
            cx.run_until_parked();
        }

        assert!(editors[0].upgrade().is_none());
        assert!(editors[1].upgrade().is_some());
        assert!(
            editors[0]
                .update(&mut cx, |editing, _| {
                    editing.theme_filter = "Late completion".into();
                })
                .is_err()
        );
    }
}
