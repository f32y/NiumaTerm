//! Editing the directories one workspace owns, and the presentation state that
//! says which of them the filesystem can currently reach.
//!
//! Path validation and availability checks are filesystem calls on a network
//! share or a sleeping disk, so every one of them runs on the background
//! executor and only its result reaches the view.

use std::{collections, fs, iter, path};

use gpui::prelude::*;
use gpui::{Context, Div, PathPromptOptions, Render, SharedString, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{
    DIALOG_BUTTON_MIN_WIDTH, DialogAction, DialogButtonProps, DialogClose, DialogFooter,
};
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use nmt_i18n::i18n;

use crate::ui::Shell;
use crate::ui::shell::agent_workspace;
use crate::workspace::{RootChange, WorkspaceId, WorkspaceRoots, root_identity};

/// A user-selected path resolved to something a workspace can own, or the
/// reason it cannot be attached.
enum Resolved {
    Directory(String),
    Unusable(String),
}

/// Resolve one picked path to an absolute existing directory. Runs on the
/// background executor: `canonicalize` and `is_dir` both hit the filesystem,
/// which can block for seconds on a disconnected share.
fn resolve_directory(path: path::PathBuf) -> Resolved {
    let display = path.display().to_string();
    let Ok(resolved) = fs::canonicalize(&path) else {
        return Resolved::Unusable(display);
    };
    if !resolved.is_dir() {
        return Resolved::Unusable(display);
    }
    Resolved::Directory(strip_verbatim_prefix(&resolved.to_string_lossy()))
}

/// Drop the `\\?\` extended-length prefix `canonicalize` adds on Windows.
/// The prefix is correct for the API but is rejected by many shells and
/// command-line tools, and it would also make the same directory look
/// different from the plain path a saved snapshot holds.
fn strip_verbatim_prefix(path: &str) -> String {
    path.strip_prefix(r"\\?\")
        .map(|stripped| match stripped.strip_prefix("UNC\\") {
            Some(unc) => format!(r"\\{unc}"),
            None => stripped.to_string(),
        })
        .unwrap_or_else(|| path.to_string())
}

/// Whether every listed directory currently resolves. Availability is
/// presentation state: a saved workspace keeps a directory it cannot reach, so
/// a disconnected drive marks a row instead of dropping it.
fn check_availability(paths: Vec<String>) -> Vec<bool> {
    paths
        .into_iter()
        .map(|path| path::Path::new(&path).is_dir())
        .collect()
}

/// Draft directory list behind the workspace-directory dialog. Owning the
/// draft rather than mutating the workspace directly is what lets Cancel leave
/// a running workspace untouched.
pub(crate) struct WorkspaceDirsEditor {
    /// `None` while a workspace being created has no directory yet, which the
    /// non-empty [`WorkspaceRoots`] invariant cannot express.
    roots: Option<WorkspaceRoots>,
    /// Parallel to `roots.ordered()`; refreshed whenever the list changes.
    available: Vec<bool>,
    /// Why the last action did nothing, shown under the list.
    notice: Option<SharedString>,
}

impl WorkspaceDirsEditor {
    pub(crate) fn new(roots: Option<WorkspaceRoots>, cx: &mut Context<Self>) -> Self {
        let mut editor = Self {
            roots,
            available: Vec::new(),
            notice: None,
        };
        editor.refresh_availability(cx);
        editor
    }

    pub(crate) fn roots(&self) -> Option<&WorkspaceRoots> {
        self.roots.as_ref()
    }

    /// The listed directories, primary first.
    fn ordered(&self) -> Vec<String> {
        self.roots
            .iter()
            .flat_map(|roots| roots.ordered())
            .map(str::to_string)
            .collect()
    }

    /// Re-check every listed directory off the UI thread. The list keeps its
    /// previous marks until the answer arrives, so opening the dialog never
    /// waits on a slow share.
    fn refresh_availability(&mut self, cx: &mut Context<Self>) {
        let paths = self.ordered();
        let expected = paths.len();

        cx.spawn(async move |editor, cx| {
            let available = cx
                .background_executor()
                .spawn(async move { check_availability(paths) })
                .await;

            let _ = editor.update(cx, |editor, cx| {
                // A second edit may have landed while the check ran; a stale
                // answer of the wrong length would mislabel rows.
                if editor.ordered().len() == expected {
                    editor.available = available;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Attach every directory the picker returned, reporting the first one
    /// that could not be attached.
    fn add_directories(&mut self, paths: Vec<path::PathBuf>, cx: &mut Context<Self>) {
        cx.spawn(async move |editor, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move { paths.into_iter().map(resolve_directory).collect::<Vec<_>>() })
                .await;

            let _ = editor.update(cx, |editor, cx| {
                let mut notice = None;
                for entry in resolved {
                    match entry {
                        Resolved::Unusable(path) => {
                            notice.get_or_insert_with(|| {
                                i18n("shell-workspace-dirs-unusable").replace("{path}", &path)
                            });
                        }
                        Resolved::Directory(path) => match &mut editor.roots {
                            Some(roots) => {
                                if roots.add(path.clone()) == RootChange::Duplicate {
                                    notice.get_or_insert_with(|| {
                                        i18n("shell-workspace-dirs-duplicate")
                                            .replace("{path}", &path)
                                    });
                                }
                            }
                            // The first usable directory of a workspace being
                            // created becomes its primary directory.
                            slot => *slot = Some(WorkspaceRoots::single(path)),
                        },
                    }
                }
                editor.notice = notice.map(SharedString::from);
                editor.refresh_availability(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn remove(&mut self, path: &str, cx: &mut Context<Self>) {
        let outcome = self
            .roots
            .as_mut()
            .map_or(RootChange::NotAttached, |roots| roots.remove(path));
        self.notice = match outcome {
            RootChange::WouldBeEmpty => Some(i18n("shell-workspace-dirs-keep-one").into()),
            _ => None,
        };
        self.refresh_availability(cx);
        cx.notify();
    }

    fn make_primary(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(roots) = self.roots.as_mut() {
            roots.make_primary(path);
        }
        self.notice = None;
        self.refresh_availability(cx);
        cx.notify();
    }

    /// One directory of the list. Returns the row's own `Div` so the caller
    /// can rule off everything but the last one without the row having to know
    /// where it sits.
    fn row(&self, index: usize, path: String, cx: &mut Context<Self>) -> Div {
        let primary = index == 0;
        // A row whose check has not returned yet reads as available; marking
        // it unavailable first would flash a warning on every edit.
        let unavailable = self.available.get(index).is_some_and(|ok| !ok);
        let promote = path.clone();
        let detach = path.clone();

        h_flex()
            .w_full()
            .py_1()
            .gap_2()
            .items_center()
            .child(
                div()
                    .id(("workspace-dir", index))
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_sm()
                    .aria_label(path.clone())
                    .child(path.clone()),
            )
            .when(unavailable, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(i18n("shell-workspace-dirs-unavailable")),
                )
            })
            .child(if primary {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(i18n("shell-workspace-dirs-primary"))
                    .into_any_element()
            } else {
                Button::new(("workspace-dir-primary", index))
                    .ghost()
                    .xsmall()
                    .label(i18n("shell-workspace-dirs-make-primary"))
                    .on_click(cx.listener(move |editor, _, _, cx| {
                        editor.make_primary(&promote, cx);
                    }))
                    .into_any_element()
            })
            .child(
                Button::new(("workspace-dir-remove", index))
                    .ghost()
                    .xsmall()
                    .label(i18n("shell-workspace-dirs-remove"))
                    .on_click(cx.listener(move |editor, _, _, cx| {
                        editor.remove(&detach, cx);
                    })),
            )
    }
}

impl Render for WorkspaceDirsEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paths = self.ordered();
        let last = paths.len().saturating_sub(1);
        let rows: Vec<_> = paths
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                self.row(index, path, cx)
                    // The rule separates two rows, so the last one has nothing
                    // below it to be separated from.
                    .when(index < last, |row| {
                        row.border_b_1().border_color(cx.theme().border)
                    })
                    .into_any_element()
            })
            .collect();

        v_flex()
            .gap_1()
            .child(div().text_sm().child(i18n("shell-workspace-dirs-label")))
            .child(v_flex().my_2().children(rows))
            .child(
                h_flex().child(
                    Button::new("workspace-dir-add")
                        .ghost()
                        .small()
                        .label(i18n("shell-workspace-dirs-add"))
                        .on_click(cx.listener(|_editor, _, _, cx| {
                            let rx = cx.prompt_for_paths(PathPromptOptions {
                                files: false,
                                directories: true,
                                multiple: true,
                                prompt: None,
                                file_types: Vec::new(),
                            });

                            cx.spawn(async move |editor, cx| {
                                if let Ok(Ok(Some(paths))) = rx.await {
                                    let _ = editor
                                        .update(cx, |editor, cx| editor.add_directories(paths, cx));
                                }
                            })
                            .detach();
                        })),
                ),
            )
            .children(
                self.notice
                    .clone()
                    .map(|notice| div().text_xs().text_color(cx.theme().danger).child(notice)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(i18n("shell-workspace-dirs-description")),
            )
    }
}

impl Shell {
    /// Open the directory editor for an existing normal workspace. Confirming
    /// replaces that workspace's directory list; the tabs and conversations
    /// already running keep the directories they started with.
    pub(crate) fn edit_workspace_dirs(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(roots) = self.workspaces.roots_of(id).cloned() else {
            return;
        };

        let editor = cx.new(|cx| WorkspaceDirsEditor::new(Some(roots), cx));
        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, window, _| {
            let editor = editor.clone();
            let content_editor = editor.clone();
            let shell = shell.clone();
            let margin_top = ((window.viewport_size().height - px(300.)) * 0.5).max(px(16.));

            dialog
                .title(i18n("shell-workspace-edit-title"))
                .overlay_closable(false)
                .margin_top(margin_top)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(i18n("shell-workspace-save"))
                        .cancel_text(i18n("shell-workspace-cancel"))
                        .show_cancel(true),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogAction::new().child(
                                Button::new("save-ws-dirs")
                                    .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                    .label(i18n("shell-workspace-save"))
                                    .primary(),
                            ),
                        )
                        .child(
                            DialogClose::new().child(
                                Button::new("cancel-ws-dirs")
                                    .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                    .label(i18n("shell-workspace-cancel")),
                            ),
                        ),
                )
                .content(move |content, _, cx| {
                    content.child(
                        v_flex().gap_2().child(content_editor.clone()).child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(i18n("shell-workspace-dirs-applies-next")),
                        ),
                    )
                })
                .on_ok(move |_, _, cx| {
                    let Some(roots) = editor.read(cx).roots().cloned() else {
                        return false;
                    };
                    shell.update(cx, |this, cx| this.replace_workspace_roots(id, roots, cx));
                    true
                })
        });
    }

    /// Adopt an edited directory list. Open Agent Tabs of this workspace pick
    /// the new list up for their next conversation; the one they are running
    /// keeps the snapshot it started with.
    fn replace_workspace_roots(
        &mut self,
        id: WorkspaceId,
        roots: WorkspaceRoots,
        cx: &mut Context<Self>,
    ) {
        self.workspaces.set_roots(id, roots);
        self.sync_agent_workspaces(id, cx);
        self.refresh_root_availability(cx);
        self.sync_session_memory(cx);
        cx.notify();
    }

    /// Hand the edited directory list to every Agent Tab of this workspace.
    /// A pane holds its configured list apart from the snapshot its running
    /// conversation was started with, so this reaches the next conversation
    /// without disturbing the one in flight.
    fn sync_agent_workspaces(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        let workspace = agent_workspace(self.workspaces.roots_of(id));
        let Some(tabs) = self.workspaces.tabs_of(id) else {
            return;
        };
        let panes: Vec<_> = tabs
            .tabs()
            .iter()
            .filter_map(|tab| tab.surface().agent().cloned())
            .collect();
        for pane in panes {
            pane.update(cx, |pane, cx| {
                pane.set_workspace(workspace.clone(), cx);
            });
        }
    }
}

impl Shell {
    /// Re-check every workspace directory off the UI thread and remember which
    /// ones the filesystem could not reach. Rendering a sidebar row or opening
    /// the New Tab menu reads the remembered answer, so neither one waits on a
    /// sleeping disk or a disconnected share.
    pub(crate) fn refresh_root_availability(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self
            .workspaces
            .summaries()
            .iter()
            .flat_map(|ws| iter::once(ws.cwd.clone()).chain(ws.additional_cwds.iter().cloned()))
            .filter(|path| !path.trim().is_empty())
            .collect();

        cx.spawn(async move |shell, cx| {
            let unreachable = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .filter(|path| !path::Path::new(path).is_dir())
                        .filter_map(|path| Some(root_key(&path)?))
                        .collect::<collections::HashSet<String>>()
                })
                .await;

            let _ = shell.update(cx, |this, cx| {
                if this.unavailable_roots != unreachable {
                    this.unavailable_roots = unreachable;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Whether `path` was reachable at the last availability check. A
    /// directory nobody has checked yet counts as available, so a row never
    /// flashes a warning it has no evidence for.
    pub(crate) fn root_is_available(&self, path: &str) -> bool {
        root_key(path).is_none_or(|key| !self.unavailable_roots.contains(&key))
    }

    /// The active workspace's directories paired with their last known
    /// availability, primary first. The New Tab menu snapshots this as it
    /// opens instead of touching the filesystem while the pointer waits.
    pub(crate) fn active_root_availability(&self) -> Vec<(String, bool)> {
        self.workspaces
            .active_roots()
            .into_iter()
            .flat_map(|roots| roots.ordered())
            .map(|path| (path.to_string(), self.root_is_available(path)))
            .collect()
    }
}

/// Comparable key for the availability set, or `None` for a placeholder path
/// that names no concrete location.
fn root_key(path: &str) -> Option<String> {
    Some(root_identity(path)?.join("/"))
}
