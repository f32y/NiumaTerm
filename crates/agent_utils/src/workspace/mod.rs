//! The directories one Agent conversation may use, in a shape every harness
//! adapter can read without knowing anything about NiumaTerm's Workspace model.

use std::path::{Component, Path};

/// The directories an Agent conversation started with. Cloned into a backend
/// at start and never mutated afterwards: editing the parent workspace must
/// not change what a running process was granted.
///
/// `primary` is `None` only where the caller has no directory to offer at all,
/// which is the same case the single-directory launch path already had to
/// handle by letting the harness pick its own default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentWorkspace {
    primary: Option<String>,
    additional: Vec<String>,
}

impl AgentWorkspace {
    /// A conversation with one directory, or none at all.
    pub fn single(primary: Option<String>) -> Self {
        Self {
            primary,
            additional: Vec::new(),
        }
    }

    /// A conversation with a primary directory and additional directories in
    /// workspace order. Additional directories without a primary one would
    /// have no `cwd` to anchor them, so they are dropped rather than silently
    /// promoted.
    pub fn new(primary: Option<String>, additional: Vec<String>) -> Self {
        match primary {
            Some(primary) => Self {
                primary: Some(primary),
                additional,
            },
            None => Self::single(None),
        }
    }

    pub fn primary(&self) -> Option<&str> {
        self.primary.as_deref()
    }

    pub fn additional(&self) -> &[String] {
        &self.additional
    }

    /// Every directory, primary first. Adapters that send a root list rely on
    /// this order being the workspace's own order.
    pub fn ordered(&self) -> impl Iterator<Item = &str> {
        self.primary
            .as_deref()
            .into_iter()
            .chain(self.additional.iter().map(String::as_str))
    }

    /// Whether this conversation was given more than one directory, which is
    /// what a primary-only harness has to disclose.
    pub fn is_multi_root(&self) -> bool {
        !self.additional.is_empty()
    }

    /// Comparable signature of the additional directories, used to keep local
    /// input history separate between workspaces that share a primary
    /// directory but attach different ones.
    ///
    /// Only the additional directories take part: the primary directory
    /// already keys the history scope through the caller's own normalization,
    /// and repeating it here would move every existing single-directory
    /// history entry to a new key. Normalization is literal so a directory
    /// that is temporarily unreachable keeps its scope.
    pub fn history_signature(&self) -> String {
        self.additional
            .iter()
            .map(|path| normalize(path))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A path reduced to a comparable spelling: separators unified, `.` segments
/// and trailing separators dropped, and case folded because Windows
/// filesystems are case-insensitive.
fn normalize(path: &str) -> String {
    let mut normalized = String::new();
    for component in Path::new(path.trim()).components() {
        if component == Component::CurDir {
            continue;
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(&component.as_os_str().to_string_lossy());
    }
    if cfg!(windows) {
        normalized.make_ascii_lowercase();
    }
    normalized
}

/// Whether a harness can use every directory an [`AgentWorkspace`] carries.
/// There is deliberately no `Default`: a new harness has to state which of
/// these it provides, so it cannot inherit another harness's answer and
/// silently claim access it does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultiRootAccess {
    /// Every attached directory reaches the harness.
    Full,
    /// The harness accepts one workspace root per session, so only the primary
    /// directory reaches it and the rest must be disclosed as unavailable.
    PrimaryOnly,
}

#[cfg(test)]
mod tests;
