//! Directory ownership for one normal workspace: a primary directory plus an
//! ordered list of additional directories.

use std::{iter, mem, path};

/// A path as comparable components: separators unified by `Path`, trailing
/// separators and `.` segments dropped by component iteration, and each
/// component lowercased because Windows filesystems are case-insensitive.
/// Literal comparison only — no symlink resolution, no filesystem access, so a
/// directory that is currently unreachable keeps its identity.
pub fn path_identity(path: &path::Path) -> Vec<String> {
    path.components()
        .map(|c| {
            let component = c.as_os_str().to_string_lossy();
            if cfg!(windows) {
                component.to_lowercase()
            } else {
                component.into_owned()
            }
        })
        .collect()
}

/// Identity of a stored root string, or `None` when the string is a
/// placeholder that does not name a concrete filesystem location.
pub fn root_identity(cwd: &str) -> Option<Vec<String>> {
    let cwd = cwd.trim();
    if cwd.is_empty() || cwd == "." {
        return None;
    }
    let comps = path_identity(path::Path::new(cwd));
    (!comps.is_empty()).then_some(comps)
}

fn same_root(a: &str, b: &str) -> bool {
    path_identity(path::Path::new(a.trim())) == path_identity(path::Path::new(b.trim()))
}

/// What a [`WorkspaceRoots`] mutation did. The caller decides how to report
/// each rejection; the value itself never touches the filesystem or the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootChange {
    Applied,
    /// The path already identifies a directory this workspace owns.
    Duplicate,
    /// The path is not one of this workspace's directories.
    NotAttached,
    /// A normal workspace always keeps at least one directory.
    WouldBeEmpty,
}

/// The directories one normal workspace owns. The primary directory is a
/// separate field rather than index zero of a list so that the non-empty
/// invariant holds by construction and persistence keeps its existing `cwd`
/// field for the primary path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRoots {
    primary: String,
    additional: Vec<String>,
}

impl WorkspaceRoots {
    /// A workspace that owns exactly one directory.
    pub fn single(primary: String) -> Self {
        Self {
            primary,
            additional: Vec::new(),
        }
    }

    /// Restore or construct from a saved list. Entries that repeat an already
    /// owned directory are dropped so a hand-edited snapshot cannot create two
    /// identities for the same path.
    pub fn new(primary: String, additional: Vec<String>) -> Self {
        let mut roots = Self::single(primary);
        for path in additional {
            roots.add(path);
        }
        roots
    }

    pub fn primary(&self) -> &str {
        &self.primary
    }

    pub fn additional(&self) -> &[String] {
        &self.additional
    }

    /// Every owned directory, primary first. Adapters and menus rely on this
    /// order being the workspace's own order.
    pub fn ordered(&self) -> impl Iterator<Item = &str> {
        iter::once(self.primary.as_str()).chain(self.additional.iter().map(String::as_str))
    }

    pub fn contains(&self, path: &str) -> bool {
        self.ordered().any(|root| same_root(root, path))
    }

    /// Append `path` as an additional directory.
    pub fn add(&mut self, path: String) -> RootChange {
        if self.contains(&path) {
            return RootChange::Duplicate;
        }
        self.additional.push(path);
        RootChange::Applied
    }

    /// Detach `path`. Removing the primary directory promotes the first
    /// additional directory and preserves the order of the rest.
    pub fn remove(&mut self, path: &str) -> RootChange {
        if same_root(&self.primary, path) {
            if self.additional.is_empty() {
                return RootChange::WouldBeEmpty;
            }
            self.primary = self.additional.remove(0);
            return RootChange::Applied;
        }
        match self
            .additional
            .iter()
            .position(|root| same_root(root, path))
        {
            Some(index) => {
                self.additional.remove(index);
                RootChange::Applied
            }
            None => RootChange::NotAttached,
        }
    }

    /// Make `path` the primary directory. The previous primary becomes the
    /// first additional directory, so every other directory keeps its relative
    /// position.
    pub fn make_primary(&mut self, path: &str) -> RootChange {
        if same_root(&self.primary, path) {
            return RootChange::Applied;
        }
        let Some(index) = self
            .additional
            .iter()
            .position(|root| same_root(root, path))
        else {
            return RootChange::NotAttached;
        };
        let promoted = self.additional.remove(index);
        let demoted = mem::replace(&mut self.primary, promoted);
        self.additional.insert(0, demoted);
        RootChange::Applied
    }
}
