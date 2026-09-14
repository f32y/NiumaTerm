//! Editing the directories one workspace owns, and the presentation state that
//! says which of them the filesystem can currently reach.
//!
//! Path validation and availability checks are filesystem calls on a network
//! share or a sleeping disk, so every one of them runs on the background
//! executor and only its result reaches the view.

use std::{collections, fs, path};

use gpui::prelude::*;
use gpui::{Context, Div, PathPromptOptions, Render, SharedString, Window, div};
use gpui_component::button::Button;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use rust_i18n::t;

use crate::ui::Shell;
use crate::workspace::{RootChange, WorkspaceRoots, root_identity};

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

/// Draft directory list behind the workspace-directory dialog. Owning the
/// draft rather than mutating the workspace directly is what lets Cancel leave
/// a running workspace untouched.
pub(crate) struct WorkspaceDirsEditor {
    /// `None` while a workspace being created has no directory yet, which the
    /// non-empty [`WorkspaceRoots`] invariant cannot express.
    roots: Option<WorkspaceRoots>,

    /// Path identities keep existing marks attached to their directories during edits.
    available: RootAvailability,

    /// Why the last action did nothing, shown under the list.
    notice: Option<SharedString>,
}

impl WorkspaceDirsEditor {
    pub(crate) fn new(roots: Option<WorkspaceRoots>, cx: &mut Context<Self>) -> Self {
        let mut editor = Self {
            roots,
            available: RootAvailability::default(),
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
        let expected = paths.clone();

        cx.spawn(async move |editor, cx| {
            let available = cx
                .background_executor()
                .spawn(async move { RootAvailability::check(paths) })
                .await;

            let _ = editor.update(cx, |editor, cx| {
                // An older check must not replace results for a different directory list.
                if editor.ordered() == expected {
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
                                t!("shell-workspace-dirs-unusable", path = &path).into_owned()
                            });
                        }

                        Resolved::Directory(path) => match &mut editor.roots {
                            Some(roots) => {
                                if roots.add(path.clone()) == RootChange::Duplicate {
                                    notice.get_or_insert_with(|| {
                                        t!("shell-workspace-dirs-duplicate", path = &path)
                                            .into_owned()
                                    });
                                }
                            }

                            // The first usable directory of a workspace being
                            // created becomes its primary directory.
                            slot => *slot = Some(WorkspaceRoots::single(path)),
                        },
                    }
                }

                editor.notice = notice.map(Into::into);
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
            RootChange::WouldBeEmpty => Some(t!("shell-workspace-dirs-keep-one").into()),
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
        let unavailable = !self.available.is_available(&path);
        let promote = path.clone();
        let detach = path.clone();

        let name = path::Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());

        h_flex()
            .w_full()
            .px_3()
            .py_3()
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::Folder)
                    .size_4()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .id(("workspace-dir", index))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .aria_label(path.clone())
                    .child(name)
                    .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx)),
            )
            .when(unavailable, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(t!("shell-workspace-dirs-unavailable")),
                )
            })
            .child(if primary {
                div()
                    .flex_shrink_0()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("shell-workspace-dirs-primary"))
                    .into_any_element()
            } else {
                Button::new(("workspace-dir-primary", index))
                    .outline()
                    .xsmall()
                    .label(t!("shell-workspace-dirs-make-primary"))
                    .on_click(cx.listener(move |editor, _, _, cx| {
                        editor.make_primary(&promote, cx);
                    }))
                    .into_any_element()
            })
            .child(
                Button::new(("workspace-dir-remove", index))
                    .outline()
                    .xsmall()
                    .label(t!("shell-workspace-dirs-remove"))
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
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().child(t!("shell-workspace-dirs-label")))
                    .child(
                        Button::new("workspace-dir-add")
                            .outline()
                            .icon(IconName::Plus)
                            .small()
                            .label(t!("shell-workspace-dirs-add"))
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
                                        let _ = editor.update(cx, |editor, cx| {
                                            editor.add_directories(paths, cx)
                                        });
                                    }
                                })
                                .detach();
                            })),
                    ),
            )
            .when(!rows.is_empty(), |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_lg()
                        .overflow_hidden()
                        .children(rows),
                )
            })
            .children(
                self.notice
                    .clone()
                    .map(|notice| div().text_xs().text_color(cx.theme().danger).child(notice)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("shell-workspace-dirs-description")),
            )
    }
}

/// Workspace directories the last background check could not reach, keyed by
/// normalized path identity. A saved directory keeps its place in its
/// workspace whether or not the filesystem can see it, so this drives
/// presentation only.
#[derive(Default)]
pub(super) struct RootAvailability {
    unavailable: collections::HashSet<String>,
}

impl RootAvailability {
    fn check(paths: Vec<String>) -> Self {
        Self {
            unavailable: paths
                .into_iter()
                .filter(|path| !path::Path::new(path).is_dir())
                .filter_map(|path| root_key(&path))
                .collect(),
        }
    }

    /// Re-check the given directories off the UI thread and remember which
    /// ones the filesystem could not reach. Rendering a sidebar row or opening
    /// the New Tab menu reads the remembered answer, so neither one waits on a
    /// sleeping disk or a disconnected share.
    pub(super) fn refresh(&mut self, paths: Vec<String>, cx: &mut Context<Shell>) {
        cx.spawn(async move |shell, cx| {
            let available = cx
                .background_executor()
                .spawn(async move { Self::check(paths) })
                .await;

            let _ = shell.update(cx, |this, cx| {
                if this.root_availability.unavailable != available.unavailable {
                    this.root_availability = available;

                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Whether `path` was reachable at the last availability check. A
    /// directory nobody has checked yet counts as available, so a row never
    /// flashes a warning it has no evidence for.
    pub(crate) fn is_available(&self, path: &str) -> bool {
        root_key(path).is_none_or(|key| !self.unavailable.contains(&key))
    }
}

/// Comparable key for the availability set, or `None` for a placeholder path
/// that names no concrete location.
fn root_key(path: &str) -> Option<String> {
    Some(root_identity(path)?.join("/"))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::ui::shell::workspace_dirs::{RootAvailability, WorkspaceDirsEditor};
    use crate::workspace::WorkspaceRoots;

    #[test]
    fn directory_marks_follow_paths_through_reordering_and_replacement() {
        let directory = tempdir().unwrap();
        let present = directory.path().to_string_lossy().into_owned();

        let missing = directory
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned();

        let replacement = directory
            .path()
            .join("replacement")
            .to_string_lossy()
            .into_owned();

        let mut editor = WorkspaceDirsEditor {
            roots: Some(WorkspaceRoots::new(present.clone(), vec![missing.clone()])),
            available: RootAvailability::check(vec![present.clone(), missing.clone()]),
            notice: None,
        };

        editor.roots.as_mut().unwrap().make_primary(&missing);

        let paths = editor.ordered();

        assert!(!editor.available.is_available(&paths[0]));
        assert!(editor.available.is_available(&paths[1]));

        editor.roots = Some(WorkspaceRoots::new(replacement.clone(), vec![present]));

        // A new path remains unmarked until its own check completes.
        assert!(editor.available.is_available(&replacement));
    }
}
